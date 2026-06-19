//! Zigbee single-radio loopback — the bladeRF transmits the precomputed ACK and
//! decodes it on its own RX (full-duplex, same band), to confirm the ACK is
//! well-formed *and* decodable by the radio itself. Like examples/zigbee `trx`,
//! but driven by the handshake's TOML flows + the plugin_host controller.
//!
//!   RX path:  sdr_head_listen.toml → zigbee_listen.toml → decoder → controller tap
//!   TX path:  the radio head's [tx] sink, fed by AckBurst replaying the ACK cf32
//!
//! Every `--period-ms` the ACK is transmitted; each decoded frame on the tap is
//! the radio hearing (and CRC-checking) its own ACK. Prints emitted vs decoded.
//!
//! Needs TX→RX coupling — antenna leakage on the full-duplex bladeRF, or an
//! attenuated TX→RX cable. Tune `--tx-gain` / `--rx-gain` so the RX hears the
//! burst without saturating.
//!
//! Run (from this dir, after ./build.sh and gen_waveforms):
//!   ../../target/release/loopback --tx-gain 50 --rx-gain 20 --period-ms 500

mod ack_burst;

use std::path::PathBuf;
use std::time::Duration;

use ack_burst::AckBurst;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use futuresdr::prelude::*;
use plugin_host::{FlowgraphController, RadioController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head_listen.toml";
const LISTEN: &str = "flows/zigbee_listen.toml";

#[derive(Parser)]
#[command(about = "Zigbee single-bladeRF loopback: TX the ACK and decode it on the same radio's RX.")]
struct Args {
    /// Precomputed ACK waveform (cf32) to transmit.
    #[arg(long, default_value = "ack_zigbee.cf32")]
    ack: PathBuf,
    /// Center frequency for both TX and RX (Hz).
    #[arg(long, default_value_t = 2.425e9)]
    freq: f64,
    /// Sample rate (Hz). Must match how the ACK was generated.
    #[arg(long, default_value_t = 4e6)]
    sample_rate: f64,
    /// TX gain (dB).
    #[arg(long, default_value_t = 40.0)]
    tx_gain: f64,
    /// RX gain (dB).
    #[arg(long, default_value_t = 20.0)]
    rx_gain: f64,
    /// Interval between transmitted ACKs (ms).
    #[arg(long, default_value_t = 500)]
    period_ms: u64,
    /// Software self-test: decode the ACK cf32 through the zigbee RX chain with
    /// NO radio (validates the waveform itself, independent of any RF path).
    #[arg(long)]
    self_test: bool,
    /// Self-test only: how many times to repeat the burst back-to-back so the
    /// demod IIR / clock recovery settle before a burst decodes.
    #[arg(long, default_value_t = 8)]
    repeats: usize,
    /// Self-test only: use a full addressed data frame instead of the imm-ACK
    /// (control — isolates "ACK too short" from "RX chain broken").
    #[arg(long)]
    control: bool,
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

/// The exact 802.15.4 immediate-ACK PHY frame the ACK waveform encodes:
/// SHR `00 00 00 A7` | PHR len | MPDU{ FCF=0x0002 | seq=0 | FCS }.
fn zigbee_imm_ack_frame() -> Vec<u8> {
    let mut mpdu = vec![0x02u8, 0x00, 0x00]; // FCF lo, FCF hi, seq
    mpdu.extend_from_slice(&zigbee_crc(&mpdu).to_le_bytes());
    let mut frame = vec![0x00u8, 0x00, 0x00, 0xA7, mpdu.len() as u8];
    frame.extend_from_slice(&mpdu);
    frame
}

/// Control: a full addressed data frame (the format examples/zigbee `Mac`
/// builds) carrying `payload`. Same SHR/CRC as `zigbee_imm_ack_frame`, just
/// longer — used to tell "ACK too short" apart from "RX chain broken".
fn zigbee_data_frame(payload: &[u8]) -> Vec<u8> {
    // FCF=0x8841 | seq | dest PAN 0x1aaa | dest addr 0xffff | src addr 0x3344.
    let mut mpdu = vec![0x41u8, 0x88, 0x00, 0xaa, 0x1a, 0xff, 0xff, 0x44, 0x33];
    mpdu.extend_from_slice(payload);
    mpdu.extend_from_slice(&zigbee_crc(&mpdu).to_le_bytes());
    let mut frame = vec![0x00u8, 0x00, 0x00, 0xA7, mpdu.len() as u8];
    frame.extend_from_slice(&mpdu);
    frame
}

/// One-shot/repeat byte source that stamps a `Tag::Id(len)` frame-start marker
/// at each frame (the tag the zigbee `IqDelay` requires; the MAC adds it in
/// normal operation). Emits `frame` back-to-back `reps` times, then finishes.
#[derive(Block)]
struct ReplayFrameSource<O: CpuBufferWriter<Item = u8> = DefaultCpuWriter<u8>> {
    frame: Vec<u8>,
    remaining: usize,
    index: usize,
    in_frame: bool,
    #[output]
    output: O,
}

impl<O: CpuBufferWriter<Item = u8>> ReplayFrameSource<O> {
    fn new(frame: Vec<u8>, reps: usize) -> Self {
        Self { frame, remaining: reps, index: 0, in_frame: false, output: O::default() }
    }
}

impl<O: CpuBufferWriter<Item = u8>> Kernel for ReplayFrameSource<O> {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        loop {
            let (out, mut tags) = self.output.slice_with_tags();
            if out.is_empty() {
                break;
            }
            if !self.in_frame {
                if self.remaining == 0 {
                    io.finished = true;
                    break;
                }
                tags.add_tag(0, Tag::Id(self.frame.len() as u64)); // frame-start, like the MAC
                self.in_frame = true;
                self.index = 0;
            } else {
                let n = std::cmp::min(out.len(), self.frame.len() - self.index);
                out[..n].copy_from_slice(&self.frame[self.index..self.index + n]);
                self.output.produce(n);
                self.index += n;
                if self.index == self.frame.len() {
                    self.in_frame = false;
                    self.remaining -= 1;
                }
            }
        }
        Ok(())
    }
}

/// Software loopback (no radio): regenerate the imm-ACK and run it through the
/// full zigbee TX *and* RX PHY in one flowgraph — exactly examples/zigbee `trx`
/// minus the SDR. `reps` back-to-back frames let the demod IIR / clock recovery
/// settle. The RX MAC logs "received frame, crc correct, payload length 5" per
/// decoded ACK.
fn self_test(frame: Vec<u8>, reps: usize, label: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use futuresdr::blocks::{Apply, ApplyIntoIter, NullSink};
    use zigbee::{ClockRecoveryMm, Decoder, IqDelay, Mac, make_nibble};

    println!(
        "self-test: {reps}× {label} ({} B) through TX PHY → RX PHY (no radio)",
        frame.len()
    );

    let mut fg = Flowgraph::new();
    let rxmac: Mac = Mac::new();
    let rxmac = fg.add_block(rxmac);

    // TX PHY
    let src: ReplayFrameSource = ReplayFrameSource::new(frame, reps);
    let modulator =
        ApplyIntoIter::<_, _, _>::new(|i: &u8| make_nibble(i & 0x0F).chain(make_nibble(i >> 4)));
    let iq: IqDelay = IqDelay::new();
    // RX PHY — FM-discriminator demod + clock recovery + decoder (as in `trx`).
    let mut last = Complex32::new(0.0, 0.0);
    let mut iir = 0.0f32;
    let alpha = 0.00016f32;
    let avg = Apply::<_, _, _>::new(move |i: &Complex32| -> f32 {
        let phase = (last.conj() * i).arg();
        last = *i;
        iir = (1.0 - alpha) * iir + alpha * phase;
        phase - iir
    });
    let mm: ClockRecoveryMm = ClockRecoveryMm::new(2.0, 0.000225, 0.5, 0.03, 0.0002);
    let decoder = Decoder::new(6);
    let snk = NullSink::<u8>::new(); // RX MAC's stream output must be sunk (see zigbee rx.rs)

    connect!(fg, src > modulator > iq > avg > mm > decoder;
                 rxmac > snk;
                 decoder | rx.rxmac);

    Runtime::new().run(fg)?;
    println!("self-test done — each 'received frame, crc correct' line above is one decoded ACK.");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();

    // Software waveform check — no radio, fully deterministic.
    if args.self_test {
        return if args.control {
            self_test(zigbee_data_frame(b"ACK"), args.repeats, "data frame")
        } else {
            self_test(zigbee_imm_ack_frame(), args.repeats, "imm-ACK")
        };
    }

    let plugin_dir = default_plugin_dir();

    // TX sink on the loopback band (full-duplex with the RX head, same device).
    let (tx_radio, tx_buf) =
        RadioController::new_sink(&plugin_dir, "", args.freq, args.sample_rate, args.tx_gain);

    let ack = AckBurst::from_files(&[args.ack.clone()], tx_buf)?;
    let burst = Duration::from_secs_f64(ack.waveform_lens()[0] as f64 / args.sample_rate);

    let (builder, mut tap_rx) = FlowgraphController::builder(plugin_dir)
        .add_head(HEAD_FLOW)
        .add_swappable(LISTEN)
        .register_radio("tx", tx_radio)
        .tap_channel(64);

    let period = Duration::from_millis(args.period_ms);
    let (freq, rx_gain, tx_gain) = (args.freq, args.rx_gain, args.tx_gain);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        for &(idx, ref path, perm) in &entries {
            if perm {
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }
        // Configurable overrides: pin RX to the loopback freq + gain.
        ctrl.set_frequency(freq).await?;
        ctrl.set_gain(rx_gain).await?;

        // ACK block in its own one-block flowgraph feeding the TX sink buffer.
        let mut ack_fg = Flowgraph::new();
        let ack_ref = ack_fg.add_block(ack);
        let ack_id: BlockId = (&ack_ref).into();
        let mut ack_handle = rt_handle.start(ack_fg).await?;

        println!(
            "Loopback @ {:.3} MHz (tx_gain {tx_gain} dB, rx_gain {rx_gain} dB, ACK burst {:.2} ms, period {} ms)",
            freq / 1e6,
            burst.as_secs_f64() * 1e3,
            period.as_millis(),
        );
        println!("TX the ACK every period; each decoded frame = the radio heard its own ACK. Ctrl-C to stop.\n");

        let mut emitted = 0u64;
        let mut decoded = 0u64;
        let mut tick = FutureExt::fuse(Timer::after(period));
        loop {
            select! {
                _ = tick => {
                    ack_handle.call(ack_id, "trigger", Pmt::Usize(0)).await?;
                    emitted += 1;
                    println!("[tx] emitted ACK #{emitted}");
                    tick = FutureExt::fuse(Timer::after(period));
                }
                msg = tap_rx.next().fuse() => match msg {
                    Some((tap, pmt)) => {
                        decoded += 1;
                        let n = match &pmt { Pmt::Blob(b) => b.len(), _ => 0 };
                        println!(
                            "[rx {tap}] decoded self-ACK ({n} B), CRC ok — loopback OK   [emitted {emitted}, decoded {decoded}]"
                        );
                    }
                    None => break,
                },
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
}
