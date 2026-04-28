//! Streaming Schmidl & Cox STF detector. Emits a buffered frame segment as
//! a `FrameCtx` message on detection.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::collections::VecDeque;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{MAX_FRAME_LEN, TS, TU};

const BOXCAR_LEN: usize = 2 * TS - TU / 4;
const DETECT_THRESHOLD: f32 = 0.56;
const MIN_DETECT_GAP: usize = 6 * TS;
const STF_LOOKBACK: usize = BOXCAR_LEN;

#[derive(Block)]
#[message_outputs(frame, corr_mag, sync_info)]
pub struct StfDetector<I = DefaultCpuReader<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
{
    #[input]
    input: I,

    ac_sum: Complex32,
    ac_ring: VecDeque<Complex32>,
    pow_sum: f32,
    pow_ring: VecDeque<f32>,
    sample_ring: VecDeque<Complex32>,
    metric_ring: VecDeque<f32>,
    since_last_det: usize,
}

impl<I> StfDetector<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            ac_sum: Complex32::new(0.0, 0.0),
            ac_ring: VecDeque::with_capacity(BOXCAR_LEN),
            pow_sum: 0.0,
            pow_ring: VecDeque::with_capacity(TS),
            sample_ring: VecDeque::with_capacity(MAX_FRAME_LEN + STF_LOOKBACK),
            metric_ring: VecDeque::with_capacity(4 * TS),
            since_last_det: MIN_DETECT_GAP,
        }
    }

    fn step_metric(&mut self, x: Complex32, x_delayed: Complex32) -> f32 {
        let prod = x * x_delayed.conj();
        self.ac_sum += prod;
        self.ac_ring.push_back(prod);
        if self.ac_ring.len() > BOXCAR_LEN {
            self.ac_sum -= self.ac_ring.pop_front().unwrap();
        }
        let p = x.norm_sqr();
        self.pow_sum += p;
        self.pow_ring.push_back(p);
        if self.pow_ring.len() > TS {
            self.pow_sum -= self.pow_ring.pop_front().unwrap();
        }
        let norm = self.pow_sum / (self.pow_ring.len() as f32).max(1.0);
        if norm <= 0.0 {
            0.0
        } else {
            self.ac_sum.norm() / norm / (self.ac_ring.len() as f32).max(1.0)
        }
    }
}

impl<I> Default for StfDetector<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I> Kernel for StfDetector<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let input_vec: Vec<Complex32> = self.input.slice().to_vec();
        let input_len = input_vec.len();
        let lag = TU / 4;

        for &x in input_vec.iter() {
            self.sample_ring.push_back(x);
            if self.sample_ring.len() > MAX_FRAME_LEN + STF_LOOKBACK {
                self.sample_ring.pop_front();
            }
            let metric = if self.sample_ring.len() > lag {
                let x_lag = self.sample_ring[self.sample_ring.len() - 1 - lag];
                self.step_metric(x, x_lag)
            } else {
                0.0
            };
            self.metric_ring.push_back(metric);
            if self.metric_ring.len() > 4 * TS {
                self.metric_ring.pop_front();
            }
            self.since_last_det = self.since_last_det.saturating_add(1);

            if self.since_last_det >= MIN_DETECT_GAP
                && metric > DETECT_THRESHOLD
                && self.metric_ring.len() >= 4 * TS
            {
                let mr = &self.metric_ring;
                let center = mr.len() - 1;
                let lo = center.saturating_sub(2 * TS);
                let is_local_max = (lo..=center).all(|i| mr[i] <= metric);
                if is_local_max && self.sample_ring.len() >= STF_LOOKBACK + MAX_FRAME_LEN {
                    // Extract frame samples starting STF_LOOKBACK + lag behind the
                    // end of the ring.
                    let end = self.sample_ring.len();
                    let frame_start = end.saturating_sub(STF_LOOKBACK + lag);
                    let frame_end = (frame_start + MAX_FRAME_LEN).min(end);
                    let mut buf = Vec::with_capacity(frame_end - frame_start);
                    for i in frame_start..frame_end {
                        buf.push(self.sample_ring[i]);
                    }
                    let ctx = FrameCtx::new(buf);

                    // Diagnostic taps (match sync_long wire format).
                    // corr_mag: last 160 samples of the detection metric.
                    let n_mag = 160usize.min(self.metric_ring.len());
                    let tail: Vec<f32> = self
                        .metric_ring
                        .iter()
                        .skip(self.metric_ring.len() - n_mag)
                        .copied()
                        .collect();
                    mio.post("corr_mag", Pmt::VecF32(tail)).await?;
                    // sync_info: [detect_idx, 0, 0, 0, metric, 0] — v2 only
                    // tracks a single detection peak, so the peak2/gap slots
                    // are zero-filled.
                    let info = vec![
                        (self.metric_ring.len() - 1) as f32,
                        0.0,
                        0.0,
                        0.0,
                        metric,
                        0.0,
                    ];
                    mio.post("sync_info", Pmt::VecF32(info)).await?;

                    mio.post("frame", Pmt::Any(Box::new(ctx))).await?;
                    self.since_last_det = 0;
                }
            }
        }

        self.input.consume(input_len);
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}
