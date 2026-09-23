//! Frame decoding: `examples/wlan`'s `Decoder`, with the interleaver and
//! SERVICE field of a [`Standard`].

use std::marker::PhantomData;

use futuresdr::prelude::*;

use crate::Deconvolve;
use crate::FrameParam;
use crate::MAX_PSDU_SIZE;
use crate::Standard;
use crate::ViterbiDecoder;

/// Decodes the data symbols of each frame the equalizer tags, and posts
/// its MPDUs with a correct FCS, without the FCS, on `rx_frames` and as
/// RFtap on `rftap`; with `invalid_frames`, also the others, whole.
///
/// `D` is how the convolutional code is undone: [`ViterbiDecoder`], or
/// [`InverseDecoder`](crate::InverseDecoder) for the code's inverse. The
/// block carries the code of that one alone, so swapping a
/// `Decoder<S, ViterbiDecoder>` for a `Decoder<S, InverseDecoder>` swaps
/// the decoding itself.
#[derive(Block)]
#[message_outputs(rx_frames, rftap)]
pub struct Decoder<S: Standard, D = ViterbiDecoder, I = DefaultCpuReader<u8>>
where
    D: Deconvolve,
    I: CpuBufferReader<Item = u8>,
{
    #[input]
    pub(crate) input: I,
    invalid_frames: bool,
    frame: Option<FrameParam>,
    copied: usize,
    rx_symbols: Vec<u8>,
    rx_bits: Vec<u8>,
    deinterleaved: Vec<u8>,
    permutation: Vec<usize>,
    decoded: Vec<u8>,
    bytes: Vec<u8>,
    deconvolve: D,
    mpdus: Vec<(Vec<u8>, bool)>,
    standard: PhantomData<fn() -> S>,
}

impl<S: Standard, D, I> Decoder<S, D, I>
where
    D: Deconvolve,
    I: CpuBufferReader<Item = u8>,
{
    pub fn new(invalid_frames: bool) -> Self {
        let mut input = I::default();
        input.set_min_items(S::N_DATA_SC);
        Self {
            input,
            invalid_frames,
            frame: None,
            copied: 0,
            rx_symbols: vec![0; S::N_DATA_SC * S::MAX_SYMBOLS],
            rx_bits: vec![0; S::MAX_CODED_BITS],
            deinterleaved: vec![0; S::MAX_CODED_BITS],
            permutation: Vec::new(),
            decoded: vec![0; S::MAX_CODED_BITS / 2 + 8],
            bytes: vec![0; MAX_PSDU_SIZE + S::SERVICE_BITS / 8],
            deconvolve: D::new(S::MAX_CODED_BITS),
            mpdus: Vec::new(),
            standard: PhantomData,
        }
    }

    /// Where the deinterleaver puts each of `n_cbps` coded bits of a symbol.
    fn permute(&mut self, n_cbps: usize, n_bpsc: usize) {
        let cols = S::INTERLEAVER_COLUMNS;
        let s = (n_bpsc / 2).max(1);
        self.permutation.clear();
        self.permutation.extend((0..n_cbps).map(|k| {
            let j = s * (k / s) + (k + cols * k / n_cbps) % s;
            cols * j - (n_cbps - 1) * (cols * j / n_cbps)
        }));
    }

    /// Decode the collected frame; returns where its PSDU is in `bytes`.
    fn decode(&mut self, frame: &FrameParam) -> std::ops::Range<usize> {
        let n_bpsc = frame.mcs.modulation.n_bpsc();
        let n_cbps = frame.n_cbps();
        let symbols = &self.rx_symbols[..frame.n_symbols * S::N_DATA_SC];
        for (bits, sym) in self.rx_bits.chunks_exact_mut(n_bpsc).zip(symbols) {
            for (k, b) in bits.iter_mut().enumerate() {
                *b = (sym >> k) & 1;
            }
        }

        self.permute(n_cbps, n_bpsc);
        for i in 0..frame.n_symbols {
            let at = i * n_cbps;
            for (k, &p) in self.permutation.iter().enumerate() {
                self.deinterleaved[at + p] = self.rx_bits[at + k];
            }
        }

        self.deconvolve.decode(
            frame.mcs.rate,
            frame.n_symbols,
            n_cbps,
            frame.n_data_bits,
            &self.deinterleaved,
            &mut self.decoded,
        );

        // Descramble; the first seven bits are the scrambler's state.
        let service = S::SERVICE_BITS / 8;
        let end = service + frame.psdu_size;
        self.bytes[..end].fill(0);
        let mut state = 0u8;
        for i in 0..7 {
            if self.decoded[i] > 0 {
                state |= 1 << (6 - i);
            }
        }
        for i in 7..end * 8 {
            let feedback = ((state >> 6) ^ (state >> 3)) & 1;
            let bit = feedback ^ (self.decoded[i] & 1);
            self.bytes[i / 8] |= bit << (i % 8);
            state = ((state << 1) & 0x7e) | feedback;
        }
        service..end
    }
}

fn rftap(frame: &[u8]) -> Vec<u8> {
    let mut rftap = Vec::with_capacity(frame.len() + 12);
    rftap.extend_from_slice(b"RFta");
    rftap.extend_from_slice(&3u16.to_le_bytes());
    rftap.extend_from_slice(&1u16.to_le_bytes());
    // Data link type: 802.11.
    rftap.extend_from_slice(&105u32.to_le_bytes());
    rftap.extend_from_slice(frame);
    rftap
}

impl<S: Standard, D, I> Default for Decoder<S, D, I>
where
    D: Deconvolve,
    I: CpuBufferReader<Item = u8>,
{
    fn default() -> Self {
        Self::new(false)
    }
}

impl<S: Standard, D, I> Kernel for Decoder<S, D, I>
where
    D: Deconvolve,
    I: CpuBufferReader<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let nd = S::N_DATA_SC;
        let (input, in_tags) = self.input.slice_with_tags();

        let mut input = input;
        let mut next_tag = None;
        let tag = in_tags.iter().find_map(|t| match &t.tag {
            Tag::NamedAny(name, any) if name == "wifi_start" => Some((t.index, any)),
            _ => None,
        });
        if let Some((index, any)) = tag {
            if index == 0 {
                if self.frame.is_some() {
                    debug!("{} decoder: frame not complete, canceling", S::NAME);
                }
                self.frame = any
                    .downcast_ref::<FrameParam>()
                    .filter(|f| f.fits::<S>())
                    .cloned();
                self.copied = 0;
            } else {
                input = &input[..index];
                next_tag = Some(index);
            }
        }

        let max_i = input.len() / nd;
        let mut i = 0;
        let mut decoded = None;
        while i < max_i {
            let Some(frame) = self.frame.as_ref() else {
                // Symbols of no frame.
                i = max_i;
                break;
            };
            let at = self.copied * nd;
            self.rx_symbols[at..at + nd].copy_from_slice(&input[i * nd..(i + 1) * nd]);
            i += 1;
            self.copied += 1;
            if self.copied == frame.n_symbols {
                decoded = self.frame.take();
                break;
            }
        }
        self.input.consume(i * nd);

        if let Some(frame) = decoded {
            let psdu = self.decode(&frame);
            S::mpdus(&frame, &self.bytes[psdu], &mut self.mpdus);
            for (mpdu, ok) in self.mpdus.drain(..) {
                // An empty MPDU is not a frame. It is what a signal field
                // decoded from noise declares, and with `invalid_frames` no
                // FCS rejects it any more: posting it turns every detection
                // the noise triggers into a frame (thousands a second on a
                // quiet channel).
                if (ok || self.invalid_frames) && !mpdu.is_empty() {
                    mo.post("rftap", Pmt::Blob(rftap(&mpdu))).await?;
                    mo.post("rx_frames", Pmt::Blob(mpdu)).await?;
                }
            }
            if i < max_i {
                io.call_again = true;
            }
        }
        if next_tag == Some(i * nd) {
            io.call_again = true;
        }
        if self.input.finished() && next_tag.is_none() && i == max_i {
            mo.post("rx_frames", Pmt::Finished).await?;
            io.finished = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::A;
    use crate::Ah;

    /// The deinterleaver undoes the interleaver of IEEE 802.11-2020
    /// (17.3.5.7 and 23.3.8.8).
    fn interleaved<S: Standard>(n_cbps: usize, n_bpsc: usize) -> Vec<usize> {
        let cols = S::INTERLEAVER_COLUMNS;
        let s = (n_bpsc / 2).max(1);
        (0..n_cbps)
            .map(|k| {
                let i = (n_cbps / cols) * (k % cols) + k / cols;
                s * (i / s) + (i + n_cbps - (cols * i / n_cbps)) % s
            })
            .collect()
    }

    #[test]
    fn deinterleaving_undoes_interleaving() {
        fn check<S: Standard>() {
            let mut decoder = Decoder::<S>::new(false);
            for n_bpsc in [1, 2, 4, 6] {
                let n_cbps = n_bpsc * S::N_DATA_SC;
                decoder.permute(n_cbps, n_bpsc);
                let forward = interleaved::<S>(n_cbps, n_bpsc);
                for k in 0..n_cbps {
                    // Coded bit `k` is sent as bit `forward[k]`, which the
                    // deinterleaver puts back at `k`.
                    assert_eq!(decoder.permutation[forward[k]], k, "{n_bpsc} {k}");
                }
            }
        }
        check::<A>();
        check::<Ah>();
    }
}
