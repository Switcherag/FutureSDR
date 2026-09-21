//! Short training field detector: `examples/wlan`'s `SyncShort`, and the
//! `Delay`, `MovingAverage`s and `Combine`s that feed it there.

use std::marker::PhantomData;
use std::ops::Add;
use std::ops::Sub;

use futuresdr::prelude::*;

use crate::Rotation;
use crate::Standard;

/// Normalized autocorrelation above which a short training field is seen.
pub const THRESHOLD: f32 = 0.56;
/// Samples between exact recomputations of the running sums.
pub(crate) const REFRESH: usize = 4096;

/// Sum of the last `len` values pushed, as `examples/wlan`'s
/// `MovingAverage` computes it, but in double precision and exactly zero
/// over zeros: the rounding errors a running sum leaves behind a strong
/// burst would otherwise make up a correlation in the silence after it.
pub(crate) struct Window<T> {
    ring: Box<[T]>,
    /// Where the next value goes; holds the oldest one.
    next: usize,
    /// Sum of the newest `len - 1` values.
    sum: T,
    /// Values in the ring that are not zero.
    nonzero: usize,
}

impl<T> Window<T>
where
    T: Copy + Default + PartialEq + Add<Output = T> + Sub<Output = T>,
{
    pub(crate) fn new(len: usize) -> Self {
        Self {
            ring: vec![T::default(); len].into_boxed_slice(),
            next: 0,
            sum: T::default(),
            nonzero: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, v: T) -> T {
        let zero = T::default();
        self.nonzero += (v != zero) as usize;
        self.nonzero -= (self.ring[self.next] != zero) as usize;
        self.ring[self.next] = v;
        let out = if self.nonzero == 0 {
            zero
        } else {
            self.sum + v
        };
        self.next += 1;
        if self.next == self.ring.len() {
            self.next = 0;
        }
        self.sum = out - self.ring[self.next];
        out
    }

    pub(crate) fn refresh(&mut self) {
        let len = self.ring.len();
        self.sum = (1..len).fold(T::default(), |s, k| s + self.ring[(self.next + k) % len]);
    }
}

/// The detector's view of one sample.
struct Point {
    /// The sample `STF_DELAY` earlier, which is what the block passes on.
    delayed: Complex32,
    /// Sum of `x[n] * conj(x[n - STF_DELAY])` over `STF_CORR_WIN` samples.
    corr: Complex64,
    /// Power of the last `STF_POWER_WIN` samples; zero until both sums are
    /// complete.
    power: f64,
}

impl Point {
    /// Whether `|corr| / power` is above `threshold`.
    #[inline(always)]
    fn above(&self, threshold: f32) -> bool {
        let limit = threshold as f64 * self.power;
        self.power > 1.0e-12 && self.corr.norm_sqr() > limit * limit
    }
}

struct Detector {
    delay: Box<[Complex32]>,
    delay_next: usize,
    power: Window<f64>,
    corr: Window<Complex64>,
    /// Samples pushed.
    n: usize,
    /// Samples until both windows are full.
    filling: usize,
}

fn wide(x: Complex32) -> Complex64 {
    Complex64::new(x.re as f64, x.im as f64)
}

impl Detector {
    fn new(delay: usize, corr: usize, power: usize) -> Self {
        Self {
            delay: vec![Complex32::default(); delay].into_boxed_slice(),
            delay_next: 0,
            power: Window::new(power),
            corr: Window::new(corr),
            n: 0,
            filling: corr.max(power) - 1,
        }
    }

    #[inline(always)]
    fn push(&mut self, x: Complex32) -> Point {
        if self.n.is_multiple_of(REFRESH) {
            self.power.refresh();
            self.corr.refresh();
        }
        self.n += 1;
        let delayed = std::mem::replace(&mut self.delay[self.delay_next], x);
        self.delay_next += 1;
        if self.delay_next == self.delay.len() {
            self.delay_next = 0;
        }
        let mut power = self.power.push(wide(x).norm_sqr());
        let corr = self.corr.push(wide(x) * wide(delayed).conj());
        if self.filling > 0 {
            // The moving averages emit zeros until they are full.
            self.filling -= 1;
            power = 0.0;
        }
        Point {
            delayed,
            corr,
            power,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum State {
    Search,
    Found,
    Copy {
        n: usize,
        rotation: Rotation,
        above: bool,
    },
}

/// What the detector makes of one sample.
pub(crate) enum Feed {
    /// Nothing to pass on.
    Skip,
    /// Pass this on; the first sample of a frame comes with its frequency
    /// offset, for the `wifi_start` tag.
    Out(Complex32, Option<f32>),
}

/// The frame logic of `examples/wlan`'s `SyncShort`: a frame starts on two
/// samples in a row above the threshold, and its samples are passed on,
/// corrected by the frequency offset the correlation shows then.
pub(crate) struct Machine<S: Standard> {
    threshold: f32,
    state: State,
    pending_tag: Option<f32>,
    standard: PhantomData<fn() -> S>,
}

impl<S: Standard> Machine<S> {
    pub(crate) fn new(threshold: f32) -> Self {
        Self {
            threshold,
            state: State::Search,
            pending_tag: None,
            standard: PhantomData,
        }
    }

    pub(crate) fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Copy from the sample after this one on, which starts a frame.
    fn start(&mut self, corr: Complex64) {
        let f_offset = (-corr.arg() / S::STF_DELAY as f64) as f32;
        self.state = State::Copy {
            n: 0,
            rotation: Rotation::new(f_offset),
            above: false,
        };
        self.pending_tag = Some(f_offset);
    }

    /// The sample `STF_DELAY` before the newest, `delayed`, with the
    /// correlation `corr` and whether its normalized magnitude is `above`
    /// the threshold.
    #[inline(always)]
    pub(crate) fn feed(&mut self, delayed: Complex32, corr: Complex64, above: bool) -> Feed {
        match &mut self.state {
            State::Copy {
                n,
                rotation,
                above: was_above,
            } => {
                if above && *was_above && *n > S::MIN_GAP {
                    // Another frame starts.
                    self.start(corr);
                    return Feed::Skip;
                }
                *was_above = above;
                let tag = if *n == 0 {
                    self.pending_tag.take()
                } else {
                    None
                };
                let out = delayed * rotation.next();
                *n += 1;
                if *n == S::MAX_SAMPLES {
                    self.state = State::Search;
                }
                Feed::Out(out, tag)
            }
            State::Search => {
                if above {
                    self.state = State::Found;
                }
                Feed::Skip
            }
            State::Found => {
                if above {
                    self.start(corr);
                } else {
                    self.state = State::Search;
                }
                Feed::Skip
            }
        }
    }
}

/// Passes on the samples of each frame, from its short training field on,
/// corrected by the frequency offset the field shows, the first one tagged
/// `wifi_start` with that offset.
#[derive(Block)]
pub struct SyncShort<S: Standard, I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    pub(crate) input: I,
    #[output]
    pub(crate) output: O,
    detector: Detector,
    machine: Machine<S>,
}

impl<S: Standard, I, O> SyncShort<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new(threshold: f32) -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            detector: Detector::new(S::STF_DELAY, S::STF_CORR_WIN, S::STF_POWER_WIN),
            machine: Machine::new(threshold),
        }
    }
}

impl<S: Standard, I, O> Default for SyncShort<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    fn default() -> Self {
        Self::new(THRESHOLD)
    }
}

impl<S: Standard, I, O> Kernel for SyncShort<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let input = self.input.slice();
        let input_len = input.len();
        let (out, mut tags) = self.output.slice_with_tags();

        let threshold = self.machine.threshold();
        let mut i = 0;
        let mut o = 0;
        while i < input_len && o < out.len() {
            let p = self.detector.push(input[i]);
            i += 1;
            if self.detector.n <= S::STF_WARMUP {
                continue;
            }
            let above = p.above(threshold);
            if let Feed::Out(x, tag) = self.machine.feed(p.delayed, p.corr, above) {
                if let Some(f_offset) = tag {
                    tags.add_tag(o, Tag::NamedF32("wifi_start".to_string(), f_offset));
                }
                out[o] = x;
                o += 1;
            }
        }

        self.input.consume(i);
        self.output.produce(o);
        if self.input.finished() && i == input_len {
            io.finished = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_rng::Rng;
    use crate::tests::normal;

    /// `examples/wlan`'s chain, block by block, on a whole signal.
    fn chain(x: &[Complex32], d: usize, wc: usize, wp: usize) -> Vec<(Complex32, Complex32, f32)> {
        let moving_sum = |v: &[Complex32], len: usize| -> Vec<Complex32> {
            (0..v.len())
                .map(|n| {
                    if n + 1 < len {
                        Complex32::default()
                    } else {
                        v[n + 1 - len..=n].iter().sum()
                    }
                })
                .collect()
        };
        let delayed: Vec<Complex32> = (0..x.len())
            .map(|n| {
                if n < d {
                    Complex32::default()
                } else {
                    x[n - d]
                }
            })
            .collect();
        let power: Vec<Complex32> = x
            .iter()
            .map(|x| Complex32::new(x.norm_sqr(), 0.0))
            .collect();
        let products: Vec<Complex32> = x.iter().zip(&delayed).map(|(a, b)| a * b.conj()).collect();
        let power = moving_sum(&power, wp);
        let corr = moving_sum(&products, wc);
        (0..x.len())
            .map(|n| {
                let ratio = if power[n].re > 1.0e-12 {
                    corr[n].norm() / power[n].re
                } else {
                    0.0
                };
                (delayed[n], corr[n], ratio)
            })
            .collect()
    }

    #[test]
    fn detector_computes_what_the_chain_computes() {
        let mut rng = Rng::new(7);
        for (d, wc, wp) in [(16, 48, 64), (32, 96, 128), (3, 7, 5)] {
            // Bursts and silences, so the sums fall back to zero too.
            let x: Vec<Complex32> = (0..3 * REFRESH)
                .map(|n| {
                    if (n / 700) % 3 == 1 {
                        Complex32::default()
                    } else {
                        Complex32::new(normal(&mut rng), normal(&mut rng)) * (1 + n % 5) as f32
                    }
                })
                .collect();
            let want = chain(&x, d, wc, wp);
            let mut det = Detector::new(d, wc, wp);
            for (n, (x, want)) in x.iter().zip(want).enumerate() {
                let p = det.push(*x);
                assert_eq!(p.delayed, want.0, "{n}");
                let corr = Complex32::new(p.corr.re as f32, p.corr.im as f32);
                if n + 1 >= wc {
                    // Before, the chain's sum is zero and the block ignores
                    // the detector's.
                    let scale = want.1.norm().max(1.0);
                    let err = (corr - want.1).norm();
                    assert!(err <= 2e-3 * scale, "{n}: {corr} {}", want.1);
                }
                let ratio = if p.power > 1e-12 {
                    corr.norm() / p.power as f32
                } else {
                    0.0
                };
                assert!((ratio - want.2).abs() <= 1e-3, "{n}: {ratio} {}", want.2);
                for threshold in [0.3, 0.56, 0.9] {
                    if (want.2 - threshold).abs() > 1e-3 {
                        assert_eq!(p.above(threshold), want.2 > threshold, "{n}");
                    }
                }
            }
        }
    }
}
