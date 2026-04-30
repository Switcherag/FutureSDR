//! Frame context passed as `Pmt::Any(Box<FrameCtx>)` between v2 blocks.

use futuresdr::num_complex::Complex32;

use crate::FrameParam;

/// Accumulates per-frame state as it flows through the v2 pipeline.
#[derive(Clone)]
pub struct FrameCtx {
    /// Buffered time-domain frame samples starting at (estimated) STF start.
    pub samples: Vec<Complex32>,
    /// Cumulative CFO correction already applied to `samples` (rad/sample).
    pub cfo: f32,
    /// Residual fractional STO (samples) to be applied as a freq-domain ramp.
    pub sto_frac: f32,
    /// FFT_SIZE-bin channel estimate (FFT-shifted, DC at index DC_INDEX). Empty until the
    /// ChannelEstimator runs.
    pub h_est: Vec<Complex32>,
    /// Parsed SIG field. `None` until SigDecoder runs successfully.
    pub frame_param: Option<FrameParam>,
}

impl FrameCtx {
    pub fn new(samples: Vec<Complex32>) -> Self {
        Self {
            samples,
            cfo: 0.0,
            sto_frac: 0.0,
            h_est: Vec::new(),
            frame_param: None,
        }
    }
}
