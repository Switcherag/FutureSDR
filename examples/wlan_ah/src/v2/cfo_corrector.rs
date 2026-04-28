//! Coarse + fine CFO estimation (from STF Tcp-lag and Ts-lag autocorrelation),
//! applied in-place to the buffered frame samples.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{TCP, TS};

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(frame)]
pub struct CfoCorrector {}

impl CfoCorrector {
    pub fn new() -> Self {
        Self {}
    }

    async fn frame(
        &mut self,
        _io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        if let Pmt::Any(a) = &p {
            if let Some(ctx) = a.downcast_ref::<FrameCtx>() {
                let mut ctx = ctx.clone();
                if ctx.samples.len() < 2 * TS {
                    return Ok(Pmt::Null);
                }
                // Coarse CFO from Tcp-lag STF autocorrelation.
                let y = &ctx.samples[0..2 * TS];
                let mut stf_corr = Complex32::new(0.0, 0.0);
                for i in TCP..2 * TS {
                    stf_corr += y[i] * y[i - TCP].conj();
                }
                let coarse = stf_corr.arg() / (TCP as f32);

                // Apply coarse correction over the full frame.
                for (n, s) in ctx.samples.iter_mut().enumerate() {
                    *s *= Complex32::from_polar(1.0, -coarse * (n as f32));
                }

                // Fine CFO from Ts-lag autocorrelation on corrected samples.
                let y = &ctx.samples[0..2 * TS];
                let mut fine_corr = Complex32::new(0.0, 0.0);
                for i in TS..2 * TS {
                    fine_corr += y[i] * y[i - TS].conj();
                }
                let fine = fine_corr.arg() / (TS as f32);
                for (n, s) in ctx.samples.iter_mut().enumerate() {
                    *s *= Complex32::from_polar(1.0, -fine * (n as f32));
                }
                ctx.cfo = coarse + fine;

                mio.post("frame", Pmt::Any(Box::new(ctx))).await?;
            }
        }
        Ok(Pmt::Null)
    }
}

impl Default for CfoCorrector {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for CfoCorrector {}
