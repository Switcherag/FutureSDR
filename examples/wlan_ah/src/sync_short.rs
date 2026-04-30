use futuresdr::prelude::*;

use crate::{CP_LEN, SYMBOL_LEN};

const MIN_GAP: usize = 480;
const MAX_SAMPLES: usize = 540 * SYMBOL_LEN;
// Rust metric is sum-of-power: M = |Σ_{288} corr| / Σ_{80} |x|².
// At a clean STF, both numerator and denominator scale linearly with |x|², so
// the metric saturates near N_corr/N_pow = 288/80 = 3.6 (NOT in 10s or 100s).
// Notebook uses mean-of-power; their 30 dB threshold = 1000 maps to ~12.5 in
// rust units only because the *peak* there is also ~80× larger. In rust units,
// 30 dB nb-equiv would be 12.5 — but real peaks are ~3.6, so we'd never trigger.
// Use a value that's safely above the noise floor (~0.3) but below the peak.
const THRESHOLD: f32 = 1.5;

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
    sample_idx: usize,
    n_locks: usize,
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
            sample_idx: 0,
            n_locks: 0,
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
        let in_sig = self.in_sig.slice();
        let in_abs = self.in_abs.slice();
        let in_cor = self.in_cor.slice();
        let in_cor_len = in_cor.len();
        let (out, mut tags) = self.output.slice_with_tags();

        let n_input = std::cmp::min(std::cmp::min(in_sig.len(), in_abs.len()), in_cor.len());

        let mut o = 0;
        let mut i = 0;

        while i < n_input && o < out.len() {
            match self.state {
                State::Search => {
                    if in_cor[i] > THRESHOLD {
                        self.state = State::Found;
                    }
                }
                State::Found => {
                    if in_cor[i] > THRESHOLD {
                        let f_offset = -in_abs[i].arg() / CP_LEN as f32;
                        self.state = State::Copy(0, f_offset, false);
                        tags.add_tag(o, Tag::NamedF32("wifi_start".to_string(), f_offset));
                        self.n_locks += 1;
                        println!(
                            "SYNC_SHORT: lock #{} sample={} M={:.3} cfo_per_sample={:.6}",
                            self.n_locks,
                            self.sample_idx + i,
                            in_cor[i],
                            f_offset
                        );
                    } else {
                        self.state = State::Search;
                    }
                }
                State::Copy(n_copied, f_offset, mut last_above_threshold) => {
                    if in_cor[i] > THRESHOLD {
                        // resync
                        if last_above_threshold && n_copied > MIN_GAP {
                            let f_offset = -in_abs[i].arg() / CP_LEN as f32;
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

                    out[o] = in_sig[i] * Complex32::from_polar(1.0, f_offset * n_copied as f32); // accum?
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
        self.sample_idx += i;

        if self.in_cor.finished() && i == in_cor_len {
            io.finished = true;
        }

        Ok(())
    }
}
