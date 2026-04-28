//! LTF-1 channel estimator — averages over the two LTF symbols with the
//! double-GI offset on the first LTF and stores the 64-bin `H_est` in the
//! frame context.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{TCP, TS, dft_shift, frac_shift_ramp, ltf_freq};
use crate::FFT_SIZE;

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(frame, channel_est)]
pub struct ChannelEstimator {}

impl ChannelEstimator {
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
                // LTF-1 is symbols index 2,3 in the preamble (STF at 0,1).
                // Symbol 0 uses a double GI (2*Tcp); symbol 1 uses standard Tcp.
                let need = 4 * TS + FFT_SIZE;
                if ctx.samples.len() < need {
                    return Ok(Pmt::Null);
                }
                let cp_corr_ltf = TCP as f32 / 2.0;
                let ltf_ref = ltf_freq();
                let mut h_est = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
                for j in 0..2 {
                    let cp_off_j = if j == 0 { 2 * TCP - TCP / 2 } else { TCP / 2 };
                    let t0 = cp_off_j + (j + 2) * TS;
                    let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
                    for n in 0..FFT_SIZE {
                        blk[n] = ctx.samples[t0 + n];
                    }
                    let fdom = dft_shift(&blk);
                    let ramp = frac_shift_ramp(cp_corr_ltf + ctx.sto_frac);
                    for k in 0..FFT_SIZE {
                        // ltf_ref is ±1 real; multiplying by it both equalizes sign
                        // and leaves magnitude unchanged.
                        h_est[k] += fdom[k] * ramp[k] * ltf_ref[k] * 0.5;
                    }
                }
                ctx.h_est = h_est;

                // Diagnostic tap: 56 active-SC channel estimate (matches the
                // wire format emitted by frame_equalizer's `channel_est`).
                let mut h_active = Vec::with_capacity(crate::N_ACTIVE_SC);
                for off in -28i32..=28 {
                    if off == 0 {
                        continue;
                    }
                    h_active.push(ctx.h_est[crate::sc(off)]);
                }
                mio.post("channel_est", Pmt::VecCF32(h_active)).await?;

                mio.post("frame", Pmt::Any(Box::new(ctx))).await?;
            }
        }
        Ok(Pmt::Null)
    }
}

impl Default for ChannelEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for ChannelEstimator {}
