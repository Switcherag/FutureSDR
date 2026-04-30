//! Per-data-symbol FFT + pilot-polyfit phase tracking + demapping. Accepts
//! fully-populated `FrameCtx` messages, demodulates all data symbols and
//! emits a `u8` stream (52 bytes per symbol, bits LSB-first) tagged with the
//! `FrameParam` at the first byte — matching the existing `Decoder` contract.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::collections::VecDeque;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{TCP, TS, dft_shift, frac_shift_ramp};
use crate::{
    DC_INDEX, FFT_SIZE, FrameParam, N_DATA_SC, PILOT_PSI, POLARITY, data_sc_for_symbol,
    pilot_sc_for_symbol,
};

struct Queued {
    bytes: Vec<u8>,
    param: FrameParam,
}

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(symbols)]
pub struct DataDemod<O = DefaultCpuWriter<u8>>
where
    O: CpuBufferWriter<Item = u8>,
{
    #[output]
    output: O,
    debug_print: bool,
    queue: VecDeque<Queued>,
    emitting: Option<(Vec<u8>, FrameParam, usize)>,
    upstream_finished: bool,
}

impl<O> DataDemod<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    pub fn new() -> Self {
        Self::new_with_debug_print(false)
    }

    pub fn new_with_debug_print(debug_print: bool) -> Self {
        Self {
            output: O::default(),
            debug_print,
            queue: VecDeque::new(),
            emitting: None,
            upstream_finished: false,
        }
    }

    async fn frame(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        if matches!(p, Pmt::Finished) {
            self.upstream_finished = true;
            mio.post("symbols", Pmt::Finished).await?;
            io.call_again = true;
            return Ok(Pmt::Null);
        }
        if let Pmt::Any(a) = &p {
            if let Some(ctx) = a.downcast_ref::<FrameCtx>() {
                if let Some((bytes, param, syms)) = demod_frame(ctx) {
                    if self.debug_print {
                        info!(
                            "[v2.data] demodulated: n_sym={} bytes={} constellation_points={}",
                            param.n_symbols(),
                            bytes.len(),
                            syms.len()
                        );
                    }
                    mio.post("symbols", Pmt::VecCF32(syms)).await?;
                    self.queue.push_back(Queued { bytes, param });
                    io.call_again = true;
                } else {
                    if self.debug_print {
                        info!("[v2.data] demod_frame returned None");
                    }
                }
            }
        }
        Ok(Pmt::Null)
    }
}

impl<O> Default for DataDemod<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<O> Kernel for DataDemod<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        loop {
            if self.emitting.is_none() {
                match self.queue.pop_front() {
                    Some(q) => self.emitting = Some((q.bytes, q.param, 0)),
                    None => break,
                }
            }
            let (bytes, param, offset) = self.emitting.as_mut().unwrap();
            let (out, mut out_tags) = self.output.slice_with_tags();
            if out.is_empty() {
                break;
            }
            let remaining = bytes.len() - *offset;
            let n = remaining.min(out.len());
            if *offset == 0 && n > 0 {
                out_tags.add_tag(
                    0,
                    Tag::NamedAny("wifi_start".to_string(), Box::new(param.clone())),
                );
            }
            out[..n].copy_from_slice(&bytes[*offset..*offset + n]);
            self.output.produce(n);
            *offset += n;
            if *offset == bytes.len() {
                self.emitting = None;
            } else {
                break;
            }
        }
        if self.upstream_finished && self.emitting.is_none() && self.queue.is_empty() {
            io.finished = true;
        }
        if self.emitting.is_some() || !self.queue.is_empty() {
            io.call_again = true;
        }
        Ok(())
    }
}

fn demod_frame(ctx: &FrameCtx) -> Option<(Vec<u8>, FrameParam, Vec<Complex32>)> {
    let fp = ctx.frame_param.clone()?;
    if ctx.h_est.len() != FFT_SIZE {
        return None;
    }
    let extra = if fp.is_long { 3usize } else { 0 };
    let n_sym = fp.n_symbols();
    if extra + n_sym > crate::MAX_SYM {
        return None;
    }

    let short_gi = fp.short_gi;
    let traveling_pilots = fp.traveling_pilots;

    let mut idx_time = 6 * TS;
    let mut all_syms: Vec<[Complex32; FFT_SIZE]> = Vec::with_capacity(extra + n_sym);
    for j in 0..extra + n_sym {
        let gi = if short_gi && j >= extra + 1 { TCP / 2 } else { TCP };
        let cp_off = gi / 2;
        if idx_time + cp_off + FFT_SIZE > ctx.samples.len() {
            return None;
        }
        let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
        for n in 0..FFT_SIZE {
            blk[n] = ctx.samples[idx_time + cp_off + n];
        }
        let fdom = dft_shift(&blk);
        let cp_corr_data = if short_gi && j >= extra + 1 {
            TCP as f32 / 4.0
        } else {
            TCP as f32 / 2.0
        };
        let ramp = frac_shift_ramp(cp_corr_data + ctx.sto_frac);
        let mut eq = [Complex32::new(0.0, 0.0); FFT_SIZE];
        for k in 0..FFT_SIZE {
            eq[k] = fdom[k] * ramp[k];
        }
        all_syms.push(eq);
        idx_time += FFT_SIZE + gi;
    }

    let mut out_bytes: Vec<u8> = Vec::with_capacity(n_sym * N_DATA_SC);
    let mut out_syms: Vec<Complex32> = Vec::with_capacity(n_sym * N_DATA_SC);
    let mod_ = fp.mcs().modulation();
    let mut accum_alpha = 0.0f32;
    let mut accum_beta = 0.0f32;

    for nsym in 0..n_sym {
        let sym = &mut all_syms[extra + nsym];
        for k in 0..FFT_SIZE {
            if ctx.h_est[k].norm_sqr() > 0.0 {
                sym[k] /= ctx.h_est[k];
            }
        }
        let pilots = pilot_sc_for_symbol(nsym, traveling_pilots);
        let psi = [
            PILOT_PSI[nsym % 4],
            PILOT_PSI[(1 + nsym) % 4],
            PILOT_PSI[(2 + nsym) % 4],
            PILOT_PSI[(3 + nsym) % 4],
        ];
        let pol = POLARITY[(nsym + 2) % 127];
        let w = [
            sym[pilots[0]] * pol * Complex32::new(psi[0], 0.0),
            sym[pilots[1]] * pol * Complex32::new(psi[1], 0.0),
            sym[pilots[2]] * pol * Complex32::new(psi[2], 0.0),
            sym[pilots[3]] * pol * Complex32::new(psi[3], 0.0),
        ];
        let xs = [
            (pilots[0] as f32) - (DC_INDEX as f32),
            (pilots[1] as f32) - (DC_INDEX as f32),
            (pilots[2] as f32) - (DC_INDEX as f32),
            (pilots[3] as f32) - (DC_INDEX as f32),
        ];
        let x_mean = (xs[0] + xs[1] + xs[2] + xs[3]) * 0.25;
        let mut residual = Complex32::new(0.0, 0.0);
        for k in 0..4 {
            let prev = accum_alpha * (xs[k] - x_mean) + accum_beta;
            residual += w[k] * Complex32::from_polar(1.0, -prev);
        }
        let resid_beta = residual.arg();
        let resid_rot = Complex32::from_polar(1.0, -resid_beta);
        let mut num = 0.0f32;
        let mut den = 0.0f32;
        for k in 0..4 {
            let prev = accum_alpha * (xs[k] - x_mean) + accum_beta;
            let wr = w[k] * Complex32::from_polar(1.0, -prev) * resid_rot;
            let dx = xs[k] - x_mean;
            num += dx * wr.im;
            den += dx * dx;
        }
        let resid_alpha = if den > 0.0 { num / den } else { 0.0 };
        accum_alpha += resid_alpha;
        accum_beta += resid_beta;
        for k in 0..FFT_SIZE {
            let off = (k as f32) - (DC_INDEX as f32);
            sym[k] *=
                Complex32::from_polar(1.0, -(accum_alpha * (off - x_mean) + accum_beta));
        }
        let data_idxs = data_sc_for_symbol(nsym, traveling_pilots);
        for &idx in data_idxs.iter() {
            out_bytes.push(mod_.demap(&sym[idx]));
            out_syms.push(sym[idx]);
        }
    }

    Some((out_bytes, fp, out_syms))
}
