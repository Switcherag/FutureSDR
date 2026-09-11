//! Fork of `examples/wlan/src/sync_short.rs`, retimed for S1G.
//!
//! The design is unchanged — threshold the normalised STF autocorrelation,
//! require two consecutive samples above it, then copy while tracking a coarse
//! CFO — only the constants move from 11a's 64-FFT at 20 MSps to S1G's
//! 128-FFT at 4 MSps. Every window doubles, so the metric's plateau height is
//! the same and `THRESHOLD` carries over unchanged.

use futuresdr::prelude::*;

use crate::{FFT_SIZE, MAX_SYM, SYMBOL_LEN};

/// Coarse-CFO lag, Tu/4. 11a used 16 at 64-FFT; S1G uses 32.
pub const STF_DELAY: usize = FFT_SIZE / 4;
/// Correlation window fed to `in_abs`, 0.75*Tu — 48 at 64-FFT, 96 here.
pub const STF_CORR_WIN: usize = 3 * FFT_SIZE / 4;
/// Power window fed to the divider, Tu — 64 at 64-FFT, 128 here.
pub const STF_POWER_WIN: usize = FFT_SIZE;

/// Minimum samples before a re-sync is honoured: six symbols, as in 11a
/// (480 = 6*80 there, 960 here).
const MIN_GAP: usize = 6 * SYMBOL_LEN;
/// Longest run copied downstream before giving up on a frame.
const MAX_SAMPLES: usize = (6 + MAX_SYM) * SYMBOL_LEN;
/// Both windows scale together, so the plateau ratio (corr/power ≈ 0.75) is
/// unchanged from 11a and so is the threshold.
const THRESHOLD: f32 = 0.56;

/// Samples to ignore after the block starts.
///
/// `in_cor` is |MA(STF_CORR_WIN)| / MA(STF_POWER_WIN), and both averages begin
/// empty. Until the power window has filled, that denominator is far too small
/// and the ratio clears THRESHOLD on noise alone — the detector then latches a
/// frame with a garbage CFO and streams up to MAX_SAMPLES (19 ms at 4 MSps)
/// before it can recover, swallowing whatever real frame arrives meanwhile.
///
/// Irrelevant to a long-lived flowgraph, which pays it once. It matters under
/// per-frame PHY swapping, where the chain is rebuilt constantly and every
/// rebuild is another chance to latch onto nothing.
const WARMUP: usize = STF_POWER_WIN + STF_CORR_WIN;

// A burst-onset guard was tried here and REMOVED — it measured worse.
//
// The idea was v2's: `in_cor` cannot tell a preamble from a carrier, because
// the STF is periodic at lag Tu/4 and so is any continuous tone, both scoring
// STF_CORR_WIN/STF_POWER_WIN = 0.75. v2 scores the correlation against the
// mean power *before* the candidate and demands a 30 dB rise, which a carrier
// can never show. Reconstructing the power here as |in_abs|/in_cor, the same
// ratio is exactly STF_CORR_WIN for a carrier of any amplitude, so the test
// becomes "beat that constant floor by a margin".
//
// It works as designed and is still not worth it. Over 10,000 frames it cut
// false SIG-A attempts 94 -> 74 and cancels 3 -> 0, but PER went 0.540% ->
// 0.730%: it rejected ~19 real frames to save ~20 false latches. v2 can look
// back from the *computed burst start* because it buffers and finds the peak
// first; this block decides while streaming, mid-STF, so the reference window
// is much harder to place — one symbol back sits inside the burst and rejects
// everything (2 frames in 165 s), three symbols back is safe but still costs
// real frames at the margins.
//
// The conclusion that matters: false triggers are not the dominant loss here.
// 94 spurious attempts per 10,000 frames cannot account for a 0.5% PER, since
// each only costs a frame when it collides with a real preamble.

#[derive(Debug)]
enum State {
    Search,
    Found,
    Copy(usize, f32, bool),
}

#[derive(Block)]
pub struct SyncShort<
    I0 = DefaultCpuReader<Complex32>,
    I1 = DefaultCpuReader<Complex32>,
    I2 = DefaultCpuReader<f32>,
    O = DefaultCpuWriter<Complex32>,
> where
    I0: CpuBufferReader<Item = Complex32>,
    I1: CpuBufferReader<Item = Complex32>,
    I2: CpuBufferReader<Item = f32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    in_sig: I0,
    #[input]
    in_abs: I1,
    #[input]
    in_cor: I2,
    #[output]
    output: O,
    state: State,
    /// Samples seen since start; the detector stays quiet below `WARMUP`.
    seen: usize,
}

impl<I0, I1, I2, O> SyncShort<I0, I1, I2, O>
where
    I0: CpuBufferReader<Item = Complex32>,
    I1: CpuBufferReader<Item = Complex32>,
    I2: CpuBufferReader<Item = f32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new() -> Self {
        Self {
            in_sig: I0::default(),
            in_abs: I1::default(),
            in_cor: I2::default(),
            output: O::default(),
            state: State::Search,
            seen: 0,
        }
    }
}

impl<I0, I1, I2, O> Default for SyncShort<I0, I1, I2, O>
where
    I0: CpuBufferReader<Item = Complex32>,
    I1: CpuBufferReader<Item = Complex32>,
    I2: CpuBufferReader<Item = f32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I0, I1, I2, O> Kernel for SyncShort<I0, I1, I2, O>
where
    I0: CpuBufferReader<Item = Complex32>,
    I1: CpuBufferReader<Item = Complex32>,
    I2: CpuBufferReader<Item = f32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _m: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        // Read before the slices are borrowed: the deferral below must not
        // stall at end-of-stream, where no further input can ever arrive.
        let cor_finished = self.in_cor.finished();
        let in_sig = self.in_sig.slice();
        let in_abs = self.in_abs.slice();
        let in_cor = self.in_cor.slice();
        let in_cor_len = in_cor.len();
        let (out, mut tags) = self.output.slice_with_tags();

        let n_input = in_sig.len().min(in_abs.len()).min(in_cor.len());

        let mut o = 0;
        let mut i = 0;

        while i < n_input && o < out.len() {
            // Hold off until both moving averages are full, so a cold start
            // cannot manufacture a detection out of an empty denominator.
            if self.seen < WARMUP {
                self.seen += 1;
                i += 1;
                continue;
            }

            match self.state {
                State::Search => {
                    if in_cor[i] > THRESHOLD {
                        self.state = State::Found;
                    }
                }
                State::Found => {
                    if in_cor[i] > THRESHOLD {
                        // A `wifi_start` tag marks the first *copied* sample, and
                        // that sample is written on the NEXT iteration. If the
                        // input chunk ends right here, `produce(o)` is called with
                        // the tag sitting at index `o` — one past the produced
                        // range — and the runtime drops it. SyncLong then never
                        // leaves `State::Broken` and discards the whole frame.
                        //
                        // So only commit the latch when this call can also copy
                        // the sample the tag points at. Otherwise break without
                        // consuming: state stays `Found`, and the next call sees
                        // the same two above-threshold samples and latches then.
                        //
                        // The odds of landing on a chunk boundary are 1/chunk, so
                        // this is invisible with the large chunks a warm flowgraph
                        // sees and dominant under per-frame swapping, where the
                        // rebuilt chain is fed in chunks of a few dozen samples.
                        if i + 1 >= n_input && !cor_finished {
                            break;
                        }
                        let f_offset = -in_abs[i].arg() / STF_DELAY as f32;
                        self.state = State::Copy(0, f_offset, false);
                        tags.add_tag(o, Tag::NamedF32("wifi_start".to_string(), f_offset));
                    } else {
                        self.state = State::Search;
                    }
                }
                State::Copy(n_copied, f_offset, mut last_above_threshold) => {
                    if in_cor[i] > THRESHOLD {
                        if last_above_threshold && n_copied > MIN_GAP {
                            // Same tag-at-the-chunk-boundary hazard as above.
                            if i + 1 >= n_input && !cor_finished {
                                break;
                            }
                            let f_offset = -in_abs[i].arg() / STF_DELAY as f32;
                            self.state = State::Copy(0, f_offset, false);
                            tags.add_tag(o, Tag::NamedF32("wifi_start".to_string(), f_offset));
                            i += 1;
                            continue;
                        } else {
                            last_above_threshold = true;
                        }
                    } else {
                        last_above_threshold = false;
                    }

                    out[o] = in_sig[i] * Complex32::from_polar(1.0, f_offset * n_copied as f32);
                    o += 1;

                    if n_copied + 1 == MAX_SAMPLES {
                        self.state = State::Search;
                    } else {
                        self.state = State::Copy(n_copied + 1, f_offset, last_above_threshold);
                    }
                }
            }
            i += 1;
        }

        self.in_sig.consume(i);
        self.in_abs.consume(i);
        self.in_cor.consume(i);
        self.output.produce(o);

        if self.in_cor.finished() && i == in_cor_len {
            io.finished = true;
        }

        Ok(())
    }
}
