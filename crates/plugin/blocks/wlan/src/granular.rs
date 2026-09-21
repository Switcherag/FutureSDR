//! The receiver front end as `examples/wlan` builds it, block by block, to
//! compare with the fused blocks:
//!
//! ```text
//!        ┌> Delay ─────────────────────────────┬──────────────> in_sig ┐
//! input ─┼> WlanMagSquared > WlanMovingSum ────│─> in1 ┐              │
//!        └> WlanMultiplyConj.in0 (in1: Delay) ─┴> WlanMovingSum ─┬> in0 WlanDivideMag > in_cor ├> WlanSyncShort
//!                                                                └──────────────> in_abs ┘
//! WlanSyncShort > WlanSyncLong > WlanFft > WlanFrameEqualizer > WlanDecoder
//! ```
//!
//! `Delay` is the basic plugin's. The sums and products are in double
//! precision, and the moving sums exact over zeros, as in the fused
//! `WlanSync` (with `examples/wlan`'s single precision sums, the rounding
//! a burst leaves behind makes up a correlation in the silence after it),
//! so both front ends see the same frames.

use std::marker::PhantomData;
use std::sync::Arc;

use futuresdr::fft;
use futuresdr::prelude::*;

use crate::Standard;
use crate::sync_short::Feed;
use crate::sync_short::Machine;
use crate::sync_short::REFRESH;
use crate::sync_short::Window;

fn wide(x: Complex32) -> Complex64 {
    Complex64::new(x.re as f64, x.im as f64)
}

/// `|x|²`, in double precision.
#[derive(Block)]
pub struct MagSquared {
    #[input]
    input: DefaultCpuReader<Complex32>,
    #[output]
    output: DefaultCpuWriter<f64>,
}

impl MagSquared {
    pub fn new() -> Self {
        Self {
            input: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
        }
    }
}

impl Default for MagSquared {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for MagSquared {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let (i_len, n) = (i.len(), i.len().min(o.len()));
        for (x, y) in i[..n].iter().zip(&mut o[..n]) {
            *y = wide(*x).norm_sqr();
        }
        self.input.consume(n);
        self.output.produce(n);
        if self.input.finished() && n == i_len {
            io.finished = true;
        }
        Ok(())
    }
}

/// `in0 · conj(in1)`, in double precision.
#[derive(Block)]
pub struct MultiplyConj {
    #[input]
    in0: DefaultCpuReader<Complex32>,
    #[input]
    in1: DefaultCpuReader<Complex32>,
    #[output]
    output: DefaultCpuWriter<Complex64>,
}

impl MultiplyConj {
    pub fn new() -> Self {
        Self {
            in0: DefaultCpuReader::default(),
            in1: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
        }
    }
}

impl Default for MultiplyConj {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for MultiplyConj {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let a = self.in0.slice();
        let b = self.in1.slice();
        let o = self.output.slice();
        let (a_len, b_len) = (a.len(), b.len());
        let n = a_len.min(b_len).min(o.len());
        for ((x, y), z) in a[..n].iter().zip(&b[..n]).zip(&mut o[..n]) {
            *z = wide(*x) * wide(*y).conj();
        }
        let finished = (self.in0.finished() && n == a_len) || (self.in1.finished() && n == b_len);
        self.in0.consume(n);
        self.in1.consume(n);
        self.output.produce(n);
        if finished {
            io.finished = true;
        }
        Ok(())
    }
}

/// Sum of the last `len` items, zero until there are `len - 1`, as
/// `examples/wlan`'s `MovingAverage` (which sums too).
#[derive(Block)]
pub struct MovingSum<T: CpuSample> {
    #[input]
    input: DefaultCpuReader<T>,
    #[output]
    output: DefaultCpuWriter<T>,
    window: Window<T>,
    /// Items still to come before the sum is complete.
    filling: usize,
    n: usize,
}

impl<T> MovingSum<T>
where
    T: CpuSample + PartialEq + std::ops::Add<Output = T> + std::ops::Sub<Output = T>,
{
    pub fn new(len: usize) -> Self {
        let len = len.max(1);
        Self {
            input: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
            window: Window::new(len),
            filling: len - 1,
            n: 0,
        }
    }
}

impl<T> Kernel for MovingSum<T>
where
    T: CpuSample + PartialEq + std::ops::Add<Output = T> + std::ops::Sub<Output = T>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let (i_len, n) = (i.len(), i.len().min(o.len()));
        for (x, y) in i[..n].iter().zip(&mut o[..n]) {
            if self.n.is_multiple_of(REFRESH) {
                self.window.refresh();
            }
            self.n += 1;
            let sum = self.window.push(*x);
            *y = if self.filling > 0 {
                self.filling -= 1;
                T::default()
            } else {
                sum
            };
        }
        self.input.consume(n);
        self.output.produce(n);
        if self.input.finished() && n == i_len {
            io.finished = true;
        }
        Ok(())
    }
}

/// `|in0| / in1`, zero where `in1` is not above 1e-12: the normalized
/// autocorrelation.
#[derive(Block)]
pub struct DivideMag {
    #[input]
    in0: DefaultCpuReader<Complex64>,
    #[input]
    in1: DefaultCpuReader<f64>,
    #[output]
    output: DefaultCpuWriter<f64>,
}

impl DivideMag {
    pub fn new() -> Self {
        Self {
            in0: DefaultCpuReader::default(),
            in1: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
        }
    }
}

impl Default for DivideMag {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for DivideMag {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let a = self.in0.slice();
        let b = self.in1.slice();
        let o = self.output.slice();
        let (a_len, b_len) = (a.len(), b.len());
        let n = a_len.min(b_len).min(o.len());
        for ((c, p), z) in a[..n].iter().zip(&b[..n]).zip(&mut o[..n]) {
            *z = if *p > 1.0e-12 { c.norm() / p } else { 0.0 };
        }
        let finished = (self.in0.finished() && n == a_len) || (self.in1.finished() && n == b_len);
        self.in0.consume(n);
        self.in1.consume(n);
        self.output.produce(n);
        if finished {
            io.finished = true;
        }
        Ok(())
    }
}

/// `examples/wlan`'s `SyncShort`: the delayed samples on `in_sig`, the
/// autocorrelation on `in_abs` and its normalized magnitude on `in_cor`;
/// passes on the samples of each frame as the fused `WlanSync` does.
#[derive(Block)]
pub struct SyncShortGranular<S: Standard> {
    #[input]
    in_sig: DefaultCpuReader<Complex32>,
    #[input]
    in_abs: DefaultCpuReader<Complex64>,
    #[input]
    in_cor: DefaultCpuReader<f64>,
    #[output]
    output: DefaultCpuWriter<Complex32>,
    machine: Machine<S>,
    n: usize,
}

impl<S: Standard> SyncShortGranular<S> {
    pub fn new(threshold: f32) -> Self {
        Self {
            in_sig: DefaultCpuReader::default(),
            in_abs: DefaultCpuReader::default(),
            in_cor: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
            machine: Machine::new(threshold),
            n: 0,
        }
    }
}

impl<S: Standard> Kernel for SyncShortGranular<S> {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let sig = self.in_sig.slice();
        let abs = self.in_abs.slice();
        let cor = self.in_cor.slice();
        let lens = [sig.len(), abs.len(), cor.len()];
        let len = lens[0].min(lens[1]).min(lens[2]);
        let (out, mut tags) = self.output.slice_with_tags();
        let threshold = self.machine.threshold() as f64;

        let mut i = 0;
        let mut o = 0;
        while i < len && o < out.len() {
            let (delayed, corr, ratio) = (sig[i], abs[i], cor[i]);
            i += 1;
            self.n += 1;
            if self.n <= S::STF_WARMUP {
                continue;
            }
            if let Feed::Out(x, tag) = self.machine.feed(delayed, corr, ratio > threshold) {
                if let Some(f_offset) = tag {
                    tags.add_tag(o, Tag::NamedF32("wifi_start".to_string(), f_offset));
                }
                out[o] = x;
                o += 1;
            }
        }
        let finished = (self.in_sig.finished() && i == lens[0])
            || (self.in_abs.finished() && i == lens[1])
            || (self.in_cor.finished() && i == lens[2]);
        self.in_sig.consume(i);
        self.in_abs.consume(i);
        self.in_cor.consume(i);
        self.output.produce(o);
        if finished {
            io.finished = true;
        }
        Ok(())
    }
}

/// FutureSDR's `Fft` block, forward, `S::FFT_SIZE` points, DC in the
/// middle (`fft_shift`), tags passed on. Planned by the shared library: a
/// plugin that plans through `rustfft` compiles all its algorithms in.
#[derive(Block)]
pub struct Fft<S: Standard> {
    #[input]
    input: DefaultCpuReader<Complex32>,
    #[output]
    output: DefaultCpuWriter<Complex32>,
    fft: Arc<dyn fft::Fft<f32>>,
    scratch: Vec<Complex32>,
    standard: PhantomData<fn() -> S>,
}

impl<S: Standard> Fft<S> {
    pub fn new() -> Self {
        let mut input = DefaultCpuReader::default();
        input.set_min_items(S::FFT_SIZE);
        let mut output = DefaultCpuWriter::default();
        output.set_min_items(S::FFT_SIZE);
        let fft = fft::forward(S::FFT_SIZE);
        Self {
            input,
            output,
            scratch: vec![Complex32::default(); fft.get_outofplace_scratch_len()],
            fft,
            standard: PhantomData,
        }
    }
}

impl<S: Standard> Default for Fft<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Standard> Kernel for Fft<S> {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let n = S::FFT_SIZE;
        let (i, in_tags) = self.input.slice_with_tags();
        let (o, mut out_tags) = self.output.slice_with_tags();
        let i_len = i.len();
        let m = i_len.min(o.len()) / n * n;
        for t in in_tags.iter().filter(|t| t.index < m) {
            out_tags.add_tag(t.index, t.tag.clone());
        }
        for (x, y) in i[..m].chunks_exact(n).zip(o[..m].chunks_exact_mut(n)) {
            // The copy is the input, which the transform may not change.
            let mut buf = x.to_vec();
            self.fft
                .process_outofplace_with_scratch(&mut buf, y, &mut self.scratch);
            y.rotate_left(n / 2);
        }
        self.input.consume(m);
        self.output.produce(m);
        if self.input.finished() && i_len - m < n {
            io.finished = true;
        }
        Ok(())
    }
}
