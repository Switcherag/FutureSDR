//! IEEE 802.11 OFDM receiver as a plugin, for two standards:
//!
//! - `A`: 802.11a/g, 20 MHz, as `examples/wlan`;
//! - `Ah`: 802.11ah (HaLow), S1G 2 MHz sampled at 4 MSps, as the `dyn`
//!   branch's receiver v6, which is `examples/wlan` moved to S1G.
//!
//! Both are one receiver whose sizes and signal fields differ, so the
//! blocks are generic over a [`Standard`] and exported once per standard:
//!
//! ```text
//! WlanSync<Ah> > WlanSyncLong<Ah> > WlanEqualizer<Ah> > WlanDecoder<Ah>
//! ```
//!
//! `WlanSync` is `examples/wlan`'s short-preamble detector with the delay
//! line and moving averages that feed it built in, and `WlanEqualizer`
//! includes the FFT. The decoder posts each MPDU with a correct FCS, without
//! the FCS, on `rx_frames`, and as RFtap on `rftap`.
//!
//! The front end is also exported as `examples/wlan` builds it, block by
//! block (see `granular`), to compare what the fusion saves:
//!
//! ```text
//! Delay, WlanMagSquared, WlanMultiplyConj, WlanMovingSum, WlanDivideMag >
//! WlanSyncShort<S> > WlanSyncLong<S> > WlanFft<S> > WlanFrameEqualizer<S> > WlanDecoder<S>
//! ```

#![allow(clippy::needless_range_loop)]

extern crate futuresdr_plugin_rt as futuresdr;

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

mod a;
mod ah;
mod decoder;
mod equalizer;
mod granular;
mod phy;
mod single;
mod sync_long;
mod sync_short;
mod tables;
mod viterbi;

pub use a::A;
pub use ah::Ah;
pub use decoder::Decoder;
pub use equalizer::FrameEqualizer;
pub use equalizer::Signal;
pub use equalizer::SymbolEqualizer;
pub use granular::DivideMag;
pub use granular::MagSquared;
pub use granular::MovingSum;
pub use granular::MultiplyConj;
pub use granular::SyncShortGranular;
pub use phy::*;
pub use single::Receiver;
pub use sync_long::SyncLong;
pub use sync_short::SyncShort;
pub use viterbi::Deconvolve;
pub use viterbi::InverseDecoder;
pub use viterbi::ViterbiDecoder;

/// What distinguishes the 802.11 OFDM variants the blocks receive.
pub trait Standard: Send + Sync + 'static {
    /// For logs.
    const NAME: &'static str;
    /// Samples of the OFDM symbol without cyclic prefix.
    const FFT_SIZE: usize;
    /// Samples of the cyclic prefix.
    const CP_LEN: usize;
    /// Samples of an OFDM symbol.
    const SYMBOL_LEN: usize = Self::FFT_SIZE + Self::CP_LEN;
    /// Lag of the short training field's autocorrelation, its period.
    const STF_DELAY: usize = Self::FFT_SIZE / 4;
    /// Samples summed for the autocorrelation.
    const STF_CORR_WIN: usize = 3 * Self::FFT_SIZE / 4;
    /// Samples summed for the power.
    const STF_POWER_WIN: usize = Self::FFT_SIZE;
    /// Samples the detector ignores after it starts.
    const STF_WARMUP: usize;
    /// Samples of a frame before a new short training field restarts it.
    const MIN_GAP: usize = 6 * Self::SYMBOL_LEN;
    /// Longest run of samples the detector passes on for one frame.
    const MAX_SAMPLES: usize;
    /// Positions searched for the long training field.
    const LTF_SEARCH: usize;
    /// Data subcarriers per OFDM symbol.
    const N_DATA_SC: usize;
    /// Columns of the data interleaver.
    const INTERLEAVER_COLUMNS: usize;
    /// Bits of the SERVICE field.
    const SERVICE_BITS: usize;
    /// Most data symbols of a frame: the largest PSDU at the lowest rate.
    const MAX_SYMBOLS: usize =
        (Self::SERVICE_BITS + 8 * MAX_PSDU_SIZE + TAIL_BITS) / (Self::N_DATA_SC / 2) + 1;
    /// Bound of the coded bits of a frame, and of twice its data bits.
    const MAX_CODED_BITS: usize =
        2 * (Self::SERVICE_BITS + 8 * MAX_PSDU_SIZE + TAIL_BITS + 6 * Self::N_DATA_SC);

    /// Channel estimation, signal field and data symbols.
    type Equalizer: SymbolEqualizer;

    /// Taps of the matched filter that finds the long training field.
    fn ltf_taps() -> Vec<Complex32>;

    /// The MPDUs of a decoded `psdu`, each with whether its FCS is correct,
    /// and without the FCS if it is.
    fn mpdus(frame: &FrameParam, psdu: &[u8], out: &mut Vec<(Vec<u8>, bool)>);
}

export_plugin! {
    name: "wlan",
    blocks: [
        {
            name: "WlanSync",
            types: [A, Ah],
            description: "Short training field detector: passes on the samples of each frame, \
                          frequency corrected, tagged `wifi_start` (setting threshold).",
            add: |s| SyncShort::<T>::new(s.get_or("threshold", sync_short::THRESHOLD)?),
        },
        {
            name: "WlanSyncLong",
            types: [A, Ah],
            description: "Long training field synchronization: the two training symbols, then \
                          each symbol without its cyclic prefix.",
            add: |_s| SyncLong::<T>::new(),
        },
        {
            name: "WlanEqualizer",
            types: [A, Ah],
            description: "FFT, channel estimation, signal field and demapping of data symbols; \
                          posts `symbols` and `channel_est`.",
            add: |_s| FrameEqualizer::<T>::new(),
        },
        {
            name: "WlanDecoder",
            types: [A, Ah],
            description: "Deinterleaving, Viterbi decoding and descrambling; posts the MPDUs on \
                          `rx_frames` and `rftap` (setting invalid_frames: also those with a \
                          wrong FCS, whole). Carries the Viterbi decoder only.",
            add: |s| Decoder::<T, ViterbiDecoder>::new(s.get_or("invalid_frames", false)?),
        },
        {
            name: "WlanHardDecoder",
            types: [A, Ah],
            description: "WlanDecoder undoing the convolutional code by its inverse instead of \
                          Viterbi decoding: no error correction, rate 1/2 only. Carries the \
                          inverse only, none of the Viterbi decoder.",
            add: |s| Decoder::<T, InverseDecoder>::new(s.get_or("invalid_frames", false)?),
        },
        {
            name: "WlanReceiver",
            types: [A, Ah],
            description: "The whole receiver in one block (WlanSync > WlanSyncLong > \
                          WlanEqualizer > WlanDecoder inside); posts on rx_frames and rftap \
                          (settings threshold, invalid_frames).",
            add: |s| Receiver::<T, ViterbiDecoder>::new(
                s.get_or("threshold", sync_short::THRESHOLD)?,
                s.get_or("invalid_frames", false)?,
            ),
        },
        {
            name: "WlanHardReceiver",
            types: [A, Ah],
            description: "WlanReceiver with WlanHardDecoder inside instead of WlanDecoder: the \
                          code undone by its inverse, no error correction, rate 1/2 only.",
            add: |s| Receiver::<T, InverseDecoder>::new(
                s.get_or("threshold", sync_short::THRESHOLD)?,
                s.get_or("invalid_frames", false)?,
            ),
        },
        // examples/wlan's front end, block by block.
        {
            name: "WlanMagSquared",
            description: "|x|² of complex samples, as f64.",
            add: |_s| MagSquared::new(),
        },
        {
            name: "WlanMultiplyConj",
            description: "in0 · conj(in1), as Complex64.",
            add: |_s| MultiplyConj::new(),
        },
        {
            name: "WlanMovingSum",
            types: [f64, Complex64],
            description: "Sum of the last `len` items, zero until the window is full.",
            add: |s| MovingSum::<T>::new(s.get("len")?),
        },
        {
            name: "WlanDivideMag",
            description: "|in0| / in1: the normalized autocorrelation.",
            add: |_s| DivideMag::new(),
        },
        {
            name: "WlanSyncShort",
            types: [A, Ah],
            description: "examples/wlan's short training field detector, fed by the blocks \
                          above on in_sig, in_abs and in_cor (setting threshold).",
            add: |s| SyncShortGranular::<T>::new(s.get_or("threshold", sync_short::THRESHOLD)?),
        },
        {
            name: "WlanFft",
            types: [A, Ah],
            description: "FutureSDR's Fft block: one symbol's points, DC in the middle.",
            add: |_s| granular::Fft::<T>::new(),
        },
        {
            name: "WlanFrameEqualizer",
            types: [A, Ah],
            description: "WlanEqualizer without the FFT, behind WlanFft.",
            add: |_s| FrameEqualizer::<T>::with_fft(false),
        },
    ]
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../host/src/test_rng.rs"]
mod test_rng;
