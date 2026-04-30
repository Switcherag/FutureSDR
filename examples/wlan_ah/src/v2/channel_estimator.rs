//! LTF-1 channel estimator — averages over the two LTF symbols with the
//! double-GI offset on the first LTF and stores the FFT_SIZE-bin `H_est` in
//! the frame context.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{TCP, TS, dft_shift, frac_shift_ramp, ltf_freq};
use crate::FFT_SIZE;

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(frame, channel_est, ltf1_eq)]
pub struct ChannelEstimator {
    debug_print: bool,
}

impl ChannelEstimator {
    pub fn new() -> Self {
        Self::new_with_debug_print(false)
    }

    pub fn new_with_debug_print(debug_print: bool) -> Self {
        Self { debug_print }
    }

    async fn frame(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        if matches!(p, Pmt::Finished) {
            mio.post("frame", Pmt::Finished).await?;
            mio.post("channel_est", Pmt::Finished).await?;
            mio.post("ltf1_eq", Pmt::Finished).await?;
            io.finished = true;
            return Ok(Pmt::Null);
        }
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
                let mut ltf_raw = [[Complex32::new(0.0, 0.0); FFT_SIZE]; 2];
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
                        ltf_raw[j][k] = fdom[k] * ramp[k] * ltf_ref[k];
                        // ltf_ref is ±1 real; multiplying by it both equalizes sign
                        // and leaves magnitude unchanged.
                        h_est[k] += ltf_raw[j][k] * 0.5;
                    }
                }
                ctx.h_est = h_est;
                if self.debug_print {
                    info!(
                        "[v2.ch] h_est ready: {} samples, sto_frac={:.3}",
                        ctx.samples.len(),
                        ctx.sto_frac
                    );
                }

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

                let mut ltf_eq = Vec::with_capacity(2 * crate::N_ACTIVE_SC);
                for sym in &ltf_raw {
                    for off in -28i32..=28 {
                        if off == 0 {
                            continue;
                        }
                        let idx = crate::sc(off);
                        let h = ctx.h_est[idx];
                        if h.norm_sqr() > 0.0 {
                            ltf_eq.push(sym[idx] / h);
                        } else {
                            ltf_eq.push(Complex32::new(0.0, 0.0));
                        }
                    }
                }
                mio.post("ltf1_eq", Pmt::VecCF32(ltf_eq)).await?;

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
