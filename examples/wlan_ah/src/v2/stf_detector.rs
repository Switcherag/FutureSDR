//! Streaming Schmidl & Cox STF detector. Emits a buffered frame segment as
//! a `FrameCtx` message on detection.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::collections::VecDeque;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{MAX_FRAME_LEN, TS, TU, frame_len_for_psdu};

const LAG: usize = TU / 4;
const BOXCAR_LEN: usize = 2 * TS - LAG;
const DETECT_WIN: usize = 6 * TS;
const LOCAL_MAX_R: usize = 2 * TS;
const NORM_WIN_LEN: usize = TS / 2;
const NEED: usize = DETECT_WIN + 2 * LOCAL_MAX_R;

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
    sample_ring: VecDeque<Complex32>,
    corr_ring: VecDeque<f32>,
    sample_base: usize,
    sample_total: usize,
    corr_base: usize,
    finished_sent: bool,
    /// Samples of lookahead required past a detection candidate, and the
    /// length of the segment emitted on detection. Also the detector's
    /// cold-start latency — see [`with_max_psdu`](Self::with_max_psdu).
    max_frame_len: usize,
}

impl<I> StfDetector<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    /// Detector sized for the worst-case PSDU the PHY supports (1500 B).
    pub fn new() -> Self {
        Self::with_frame_len(MAX_FRAME_LEN)
    }

    /// Detector sized for the largest PSDU this receiver expects, in bytes.
    ///
    /// Smaller means a shorter cold start: the detector must buffer one whole
    /// frame length before its first detection can be emitted, so this is what
    /// a rebuilt flowgraph waits through before it decodes anything. Frames
    /// longer than `psdu_bytes` will not be captured — size it to the traffic.
    pub fn with_max_psdu(psdu_bytes: usize) -> Self {
        Self::with_frame_len(frame_len_for_psdu(psdu_bytes))
    }

    fn with_frame_len(max_frame_len: usize) -> Self {
        Self {
            input: I::default(),
            ac_sum: Complex32::new(0.0, 0.0),
            ac_ring: VecDeque::with_capacity(BOXCAR_LEN),
            sample_ring: VecDeque::with_capacity(max_frame_len + NEED + BOXCAR_LEN + TS),
            corr_ring: VecDeque::with_capacity(NEED + DETECT_WIN),
            sample_base: 0,
            sample_total: 0,
            corr_base: 0,
            finished_sent: false,
            max_frame_len,
        }
    }

    fn push_corr(&mut self, x: Complex32, x_delayed: Complex32) {
        let prod = x * x_delayed.conj();
        self.ac_sum += prod;
        self.ac_ring.push_back(prod);
        if self.ac_ring.len() > BOXCAR_LEN {
            self.ac_sum -= self.ac_ring.pop_front().unwrap();
        }

        if self.ac_ring.len() == BOXCAR_LEN {
            let corr_idx = self.sample_total - 1 - LAG;
            if self.corr_ring.is_empty() {
                self.corr_base = corr_idx;
            }
            self.corr_ring.push_back(self.ac_sum.norm());
        }
    }

    fn sample_latest_exclusive(&self) -> usize {
        self.sample_base + self.sample_ring.len()
    }

    fn sample_range_available(&self, start: usize, end: usize) -> bool {
        start >= self.sample_base && end <= self.sample_latest_exclusive() && start < end
    }

    fn sample_avg_power(&self, start: usize, end: usize) -> Option<f32> {
        if !self.sample_range_available(start, end) {
            return None;
        }

        let mut sum = 0.0f32;
        for abs_idx in start..end {
            sum += self.sample_ring[abs_idx - self.sample_base].norm_sqr();
        }
        Some(sum / (end - start) as f32)
    }

    fn sample_range_to_vec(&self, start: usize, end: usize) -> Option<Vec<Complex32>> {
        if !self.sample_range_available(start, end) {
            return None;
        }

        let mut out = Vec::with_capacity(end - start);
        for abs_idx in start..end {
            out.push(self.sample_ring[abs_idx - self.sample_base]);
        }
        Some(out)
    }

    fn discard_old_samples(&mut self) {
        let lag_needed = self.sample_total.saturating_sub(LAG);
        let block_needed = if self.corr_ring.is_empty() {
            lag_needed
        } else {
            self.corr_base
                .saturating_sub(BOXCAR_LEN - 1)
                .saturating_sub(TS)
        };
        let keep_from = block_needed.min(lag_needed);

        while self.sample_base < keep_from && !self.sample_ring.is_empty() {
            self.sample_ring.pop_front();
            self.sample_base += 1;
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

        for &x in input_vec.iter() {
            self.sample_ring.push_back(x);
            self.sample_total += 1;

            if self.sample_total > LAG {
                let delayed_abs = self.sample_total - 1 - LAG;
                let delayed = self.sample_ring[delayed_abs - self.sample_base];
                self.push_corr(x, delayed);
            }

            while self.corr_ring.len() >= NEED {
                let candidate_latest_corr = self.corr_base + LOCAL_MAX_R + DETECT_WIN - 1;
                let candidate_latest_start = candidate_latest_corr.saturating_sub(BOXCAR_LEN - 1);
                if self.sample_latest_exclusive() < candidate_latest_start + self.max_frame_len {
                    break;
                }

                let mut max_val = f32::NEG_INFINITY;
                let mut max_rel = 0usize;
                for i in 0..DETECT_WIN {
                    let v = self.corr_ring[LOCAL_MAX_R + i];
                    if v > max_val {
                        max_val = v;
                        max_rel = i;
                    }
                }

                let arg_buf = LOCAL_MAX_R + max_rel;
                let abs_corr_idx = self.corr_base + arg_buf;
                let start_idx = abs_corr_idx.saturating_sub(BOXCAR_LEN - 1);

                let mut local_max = f32::NEG_INFINITY;
                for i in (arg_buf - LOCAL_MAX_R)..(arg_buf + LOCAL_MAX_R) {
                    if self.corr_ring[i] > local_max {
                        local_max = self.corr_ring[i];
                    }
                }
                let is_local_max = max_val >= local_max;

                let norm = if start_idx >= TS {
                    self.sample_avg_power(start_idx - TS, start_idx - TS + NORM_WIN_LEN)
                } else {
                    None
                };
                let max_value_norm = match norm {
                    Some(v) if v > 0.0 => max_val / v,
                    _ => f32::NAN,
                };

                if is_local_max && max_value_norm.is_finite() && max_value_norm >= 10f32.powf(30.0 / 10.0) {
                    if let Some(frame) = self.sample_range_to_vec(start_idx, start_idx + self.max_frame_len) {
                        let block_corr: Vec<f32> = self
                            .corr_ring
                            .iter()
                            .skip(LOCAL_MAX_R)
                            .take(DETECT_WIN)
                            .copied()
                            .collect();
                        let info = vec![
                            start_idx as f32,
                            abs_corr_idx as f32,
                            max_value_norm,
                            norm.unwrap_or(0.0),
                            max_val,
                            30.0,
                        ];
                        mio.post("corr_mag", Pmt::VecF32(block_corr)).await?;
                        mio.post("sync_info", Pmt::VecF32(info)).await?;
                        mio.post("frame", Pmt::Any(Box::new(FrameCtx::new(frame)))).await?;
                    }
                }

                self.corr_ring.drain(0..DETECT_WIN);
                self.corr_base += DETECT_WIN;
                self.discard_old_samples();
            }
        }

        self.input.consume(input_len);
        if self.input.finished() {
            if !self.finished_sent {
                mio.post("frame", Pmt::Finished).await?;
                mio.post("corr_mag", Pmt::Finished).await?;
                mio.post("sync_info", Pmt::Finished).await?;
                self.finished_sent = true;
            }
            io.finished = true;
        }
        Ok(())
    }
}
