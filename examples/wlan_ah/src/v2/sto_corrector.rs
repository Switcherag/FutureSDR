//! Coarse + fine STO estimation from the STF subcarriers. Integer part is
//! absorbed into the sample buffer (drop / pad leading samples); fractional
//! part is stored in the context for frequency-domain correction later.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{TCP, TS, TU, dft_shift, frac_shift_ramp, linear_slope, stf_freq};
use crate::{FFT_SIZE, sc};

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(frame, ltf_td)]
pub struct StoCorrector {}

impl StoCorrector {
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
                if ctx.samples.len() < 2 * TS + FFT_SIZE {
                    return Ok(Pmt::Null);
                }

                // Average STF frequency-domain response over the 2 STF symbols.
                let cp_offset = TCP / 2;
                let cp_corr = -(TCP as f32 / 2.0);
                let stf_ref = stf_freq();
                let mut stf_avg = [Complex32::new(0.0, 0.0); FFT_SIZE];
                for j in 0..2 {
                    let t0 = cp_offset + j * TS;
                    let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
                    for n in 0..FFT_SIZE {
                        blk[n] = ctx.samples[t0 + n];
                    }
                    let fdom = dft_shift(&blk);
                    let ramp = frac_shift_ramp(cp_corr);
                    for k in 0..FFT_SIZE {
                        if stf_ref[k].norm_sqr() > 0.0 {
                            stf_avg[k] += fdom[k] * ramp[k] * stf_ref[k].conj();
                        }
                    }
                }

                let mut stf_idxs: Vec<i32> = Vec::with_capacity(12);
                let mut stf_vals: Vec<Complex32> = Vec::with_capacity(12);
                for off in -26i32..=26 {
                    let k = sc(off);
                    if stf_ref[k].norm_sqr() > 0.0 {
                        stf_idxs.push(off);
                        stf_vals.push(stf_avg[k]);
                    }
                }

                // Coarse STO via zero-padded DFT of the compact STF response.
                let n = stf_vals.len() + 1;
                let mut padded = vec![Complex32::new(0.0, 0.0); n];
                let half = stf_vals.len() / 2;
                padded[..half].copy_from_slice(&stf_vals[..half]);
                padded[n - half..].copy_from_slice(&stf_vals[half..]);
                let mut mags = vec![0.0f32; n];
                for kk in 0..n {
                    let mut acc = Complex32::new(0.0, 0.0);
                    for tt in 0..n {
                        let angle =
                            -2.0 * std::f32::consts::PI * (kk as f32) * (tt as f32) / (n as f32);
                        acc += padded[tt] * Complex32::from_polar(1.0, angle);
                    }
                    mags[kk] = acc.norm();
                }
                let mut argmax = 0usize;
                for i in 1..n {
                    if mags[i] > mags[argmax] {
                        argmax = i;
                    }
                }
                let mut idx = argmax as i32;
                if idx as usize >= n / 2 {
                    idx -= n as i32;
                }
                let coarse_sto = -(idx as f32) / (4.0 * n as f32) * (TU as f32);

                // Fine STO via polyfit of per-SC phase across the STF subcarriers.
                let mut phases = vec![0.0f32; stf_vals.len()];
                let mut avg_vec = Complex32::new(0.0, 0.0);
                for i in 0..stf_vals.len() {
                    let rot = Complex32::from_polar(
                        1.0,
                        2.0 * std::f32::consts::PI * coarse_sto * (stf_idxs[i] as f32)
                            / (TU as f32),
                    );
                    let v = stf_vals[i] * rot;
                    avg_vec += v;
                    phases[i] = v.arg();
                }
                let avg_phase = avg_vec.arg();
                let xs: Vec<f32> = stf_idxs.iter().map(|&i| i as f32).collect();
                let ys: Vec<f32> = phases
                    .iter()
                    .map(|&pp| {
                        let mut d = pp - avg_phase;
                        while d > std::f32::consts::PI {
                            d -= 2.0 * std::f32::consts::PI;
                        }
                        while d < -std::f32::consts::PI {
                            d += 2.0 * std::f32::consts::PI;
                        }
                        d
                    })
                    .collect();
                let slope = linear_slope(&xs, &ys);
                let fine_sto = -slope / (2.0 * std::f32::consts::PI) * (TU as f32);
                let total_sto = coarse_sto + fine_sto;

                // Apply integer part by shifting the sample buffer's start.
                let sto_int = total_sto.round() as i32;
                let sto_frac = total_sto - (sto_int as f32);
                if sto_int > 0 {
                    let drop = (sto_int as usize).min(ctx.samples.len());
                    ctx.samples.drain(0..drop);
                } else if sto_int < 0 {
                    let pad = (-sto_int) as usize;
                    let mut padded =
                        vec![Complex32::new(0.0, 0.0); pad + ctx.samples.len()];
                    padded[pad..].copy_from_slice(&ctx.samples);
                    ctx.samples = padded;
                }
                ctx.sto_frac = sto_frac;

                // LTF time-domain tap: 128 samples at the LTF start (post-STO
                // correction) so the UI can eyeball symbol alignment.
                let ltf_start = 2 * TS;
                if ctx.samples.len() >= ltf_start + 128 {
                    let td: Vec<Complex32> =
                        ctx.samples[ltf_start..ltf_start + 128].to_vec();
                    mio.post("ltf_td", Pmt::VecCF32(td)).await?;
                }

                mio.post("frame", Pmt::Any(Box::new(ctx))).await?;
            }
        }
        Ok(Pmt::Null)
    }
}

impl Default for StoCorrector {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for StoCorrector {}
