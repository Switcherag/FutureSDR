//! Offline ACK-waveform precompute.
//!
//! Each ACK is generated *once* by running the repository's real TX PHY into a
//! `VectorSink` and capturing the resulting IQ. The captured `Vec<Complex32>`
//! is then handed to [`crate::ack_burst::AckBurst`] for replay. Frames come
//! straight out of the standard Zigbee / 802.11ah MACs, so they are decodable,
//! standard-form frames carrying an `ACK` payload.

use std::time::Duration;

use futuresdr::async_io::Timer;
use futuresdr::async_io::block_on;
use futuresdr::blocks::ApplyIntoIter;
use futuresdr::blocks::{Fft, FftDirection, VectorSink};
use futuresdr::prelude::*;
use futuresdr::runtime::scheduler::SmolScheduler;

use halowv2::Mac as HalowMac;
use halowv2::{Encoder, Mapper, Mcs, Prefix};
use zigbee::{IqDelay, make_nibble};

/// Time to let the PHY modulate the queued frame before we tear the graph down.
const SETTLE: Duration = Duration::from_millis(200);

/// Crop leading/trailing dead air (TX ramp / guard padding the PHY adds around
/// the frame) so the stored ACK is just the active burst plus a small guard.
/// Threshold-based, so it does not depend on the exact padding constants inside
/// `IqDelay` / `Prefix`.
fn trim(wave: Vec<Complex32>, thresh: f32, guard: usize) -> Vec<Complex32> {
    let first = wave.iter().position(|c| c.norm() > thresh);
    let last = wave.iter().rposition(|c| c.norm() > thresh);
    match (first, last) {
        (Some(a), Some(b)) => {
            let a = a.saturating_sub(guard);
            let b = (b + guard + 1).min(wave.len());
            wave[a..b].to_vec()
        }
        // Silence only (shouldn't happen) — return as-is.
        _ => wave,
    }
}

/// Run a one-shot TX flowgraph: queue `tx_pmt` on `mac_id`'s `tx` handler, let
/// the PHY modulate it, then terminate and return the IQ captured by `snk`.
fn capture(
    rt: &Runtime<SmolScheduler>,
    fg: Flowgraph,
    snk: BlockRef<VectorSink<Complex32>>,
    mac_id: BlockId,
    tx_pmt: Pmt,
) -> anyhow::Result<Vec<Complex32>> {
    let (task, mut handle) = rt.start_sync(fg)?;
    rt.spawn_background(async move {
        let _ = handle.call(mac_id, "tx", tx_pmt).await;
        Timer::after(SETTLE).await;
        let _ = handle.terminate_and_wait().await;
    });
    block_on(task)?;
    let v = snk.get()?;
    Ok(v.items().clone())
}

/// One-shot byte source that stamps a `Tag::Id(len)` frame-start marker at
/// index 0 — the tag the zigbee `IqDelay` requires to delimit a burst (the MAC
/// adds it in normal operation; a plain `VectorSource` does not). Streams
/// `items` once, then finishes.
#[derive(Block)]
struct TaggedFrameSource<O: CpuBufferWriter<Item = u8> = DefaultCpuWriter<u8>> {
    items: Vec<u8>,
    n_copied: usize,
    #[output]
    output: O,
}

impl<O: CpuBufferWriter<Item = u8>> TaggedFrameSource<O> {
    fn new(items: Vec<u8>) -> Self {
        Self { items, n_copied: 0, output: O::default() }
    }
}

impl<O: CpuBufferWriter<Item = u8>> Kernel for TaggedFrameSource<O> {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let (out, mut tags) = self.output.slice_with_tags();
        if out.is_empty() {
            return Ok(());
        }
        if self.n_copied == 0 {
            // Frame-start tag carries the byte length, exactly like zigbee Mac.
            tags.add_tag(0, Tag::Id(self.items.len() as u64));
        }
        let n = std::cmp::min(out.len(), self.items.len() - self.n_copied);
        out[..n].copy_from_slice(&self.items[self.n_copied..self.n_copied + n]);
        self.n_copied += n;
        self.output.produce(n);
        if self.n_copied == self.items.len() {
            io.finished = true;
        }
        Ok(())
    }
}

/// 802.15.4 CRC-16 (same polynomial as examples/zigbee `Mac::calc_crc`).
fn zigbee_crc(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for b in data {
        for k in 0..8 {
            let bit = if b & (1 << k) != 0 { 1 ^ (crc & 1) } else { crc & 1 };
            crc >>= 1;
            if bit != 0 {
                crc ^= 1 << 15;
                crc ^= 1 << 10;
                crc ^= 1 << 3;
            }
        }
    }
    crc
}

/// 802.15.4 (O-QPSK / DSSS) *immediate-ACK*: a minimal PHY frame
///   SHR `00 00 00 A7` | PHR len | MPDU{ FCF=0x0002 (ACK) | seq=0 | FCS }
/// fed straight through `modulator -> IqDelay`. This is ~10 bytes on air
/// (~320 µs @ 4 MSps) versus the ~19-byte addressed data frame (~620 µs) the
/// full MAC builds. `payload` is ignored (an imm-ACK carries none); kept for
/// API symmetry with [`halow_ack`].
pub fn zigbee_ack(rt: &Runtime<SmolScheduler>, _payload: &[u8]) -> anyhow::Result<Vec<Complex32>> {
    // MPDU: FCF (ack = 0x0002, little-endian), sequence number, then FCS.
    let mut mpdu = vec![0x02u8, 0x00, 0x00]; // FCF lo, FCF hi, seq = 0
    let crc = zigbee_crc(&mpdu);
    mpdu.extend_from_slice(&crc.to_le_bytes());
    // PHY frame: SHR (preamble + SFD, matching examples/zigbee Mac) + PHR + MPDU.
    let mut frame = vec![0x00u8, 0x00, 0x00, 0xA7, mpdu.len() as u8];
    frame.extend_from_slice(&mpdu);

    let mut fg = Flowgraph::new();
    let src: TaggedFrameSource = TaggedFrameSource::new(frame);
    // Same mapping as zigbee::modulator(), inline so connect! can type the chain.
    let modulator =
        ApplyIntoIter::<_, _, _>::new(|i: &u8| make_nibble(i & 0x0F).chain(make_nibble(i >> 4)));
    let iq: IqDelay = IqDelay::new();
    let snk = VectorSink::<Complex32>::new(1 << 20);

    connect!(fg, src > modulator > iq > snk);

    // VectorSource streams the frame once then finishes; `run` blocks until that
    // EOF propagates through the PHY and the graph terminates on its own.
    rt.run(fg)?;
    let v = snk.get()?;
    Ok(trim(v.items().clone(), 0.1, 16))
}

/// 802.11ah (OFDM) ACK: `MAC -> Encoder -> Mapper -> IFFT -> Prefix`.
pub fn halow_ack(
    rt: &Runtime<SmolScheduler>,
    payload: &[u8],
    mcs: Mcs,
) -> anyhow::Result<Vec<Complex32>> {
    // Short guard pads — this is a burst, not a continuous stream.
    const PAD_FRONT: usize = 256;
    const PAD_TAIL: usize = 256;

    let mut fg = Flowgraph::new();
    let mac = HalowMac::new([0x42; 6], [0x23; 6], [0xff; 6]);
    let encoder: Encoder = Encoder::new(mcs);
    let mapper: Mapper = Mapper::new();
    let fft: Fft = Fft::with_options(64, FftDirection::Inverse, true, Some((1.0f32 / 52.0).sqrt()));
    let prefix: Prefix = Prefix::new(PAD_FRONT, PAD_TAIL);
    let snk = VectorSink::<Complex32>::new(1 << 20);

    connect!(fg, mac.tx | tx.encoder);
    connect!(fg, encoder > mapper > fft > prefix > snk);

    let mac_id: BlockId = (&mac).into();
    let wave = capture(rt, fg, snk, mac_id, Pmt::Blob(payload.to_vec()))?;
    Ok(trim(wave, 0.05, 64))
}
