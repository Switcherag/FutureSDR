//! Demodulates the two SIG OFDM symbols, runs inline BCC Viterbi (rate-1/2,
//! G0=171, G1=133) on the 96 coded bits, checks CRC-4, and parses the SIG
//! field into a `FrameParam`. Emits the updated context downstream only when
//! the CRC passes.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::v2::ctx::FrameCtx;
use crate::v2::helpers::{TCP, TS, dft_shift, frac_shift_ramp};
use crate::{FFT_SIZE, FrameParam, Mcs, crc4, sc, sig_data_sc};

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(frame, preamble_symbols, preamble_symbols2)]
pub struct SigDecoder {}

impl SigDecoder {
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
                if ctx.h_est.len() != FFT_SIZE {
                    return Ok(Pmt::Null);
                }
                let need = 6 * TS + FFT_SIZE;
                if ctx.samples.len() < need {
                    return Ok(Pmt::Null);
                }

                let cp_off_sig = TCP / 2;
                let cp_corr_sig = TCP as f32 / 2.0;
                let mut sig_raw = [[Complex32::new(0.0, 0.0); FFT_SIZE]; 2];
                for j in 0..2 {
                    let t0 = cp_off_sig + (j + 4) * TS;
                    let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
                    for n in 0..FFT_SIZE {
                        blk[n] = ctx.samples[t0 + n];
                    }
                    let fdom = dft_shift(&blk);
                    let ramp = frac_shift_ramp(cp_corr_sig + ctx.sto_frac);
                    for k in 0..FFT_SIZE {
                        sig_raw[j][k] = fdom[k] * ramp[k];
                    }
                }

                let sig_idxs = sig_data_sc();
                let sig_scale = (48.0f32 / 56.0).sqrt();
                let mut sig_eq = [[Complex32::new(0.0, 0.0); 48]; 2];
                for j in 0..2 {
                    for (i, &idx) in sig_idxs.iter().enumerate() {
                        let h = ctx.h_est[idx];
                        if h.norm_sqr() > 0.0 {
                            sig_eq[j][i] = sig_raw[j][idx] / h * sig_scale;
                        }
                    }
                }

                // Diagnostic tap: equalized SIG subcarriers for both symbols
                // (48 points each, VecCF32) — feeds panels ⑦/⑧ of the UI.
                mio.post(
                    "preamble_symbols",
                    Pmt::VecCF32(sig_eq[0].to_vec()),
                )
                .await?;
                mio.post(
                    "preamble_symbols2",
                    Pmt::VecCF32(sig_eq[1].to_vec()),
                )
                .await?;

                // S1G_SHORT (BPSK on imag axis both symbols) vs S1G_LONG
                // (BPSK imag for sym0, real for sym1).
                let (re_pow, im_pow) = sig_eq[1]
                    .iter()
                    .fold((0.0f32, 0.0f32), |(r, i), c| {
                        (r + c.re * c.re, i + c.im * c.im)
                    });
                let is_long = re_pow > im_pow;

                let mut sig_bits = [0u8; 96];
                if is_long {
                    for i in 0..48 {
                        sig_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
                        sig_bits[48 + i] = (sig_eq[1][i].re > 0.0) as u8;
                    }
                } else {
                    for i in 0..48 {
                        sig_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
                        sig_bits[48 + i] = (sig_eq[1][i].im > 0.0) as u8;
                    }
                }

                let mut deitl = [0u8; 96];
                for blk in 0..2 {
                    let offset = blk * 48;
                    for k in 0..48 {
                        let perm = 3 * (k % 16) + k / 16;
                        deitl[offset + perm] = sig_bits[offset + k];
                    }
                }

                let sig_info = match viterbi_decode_48(&deitl) {
                    Some(v) => v,
                    None => return Ok(Pmt::Null),
                };
                let crc_calc = crc4(&sig_info[0..38]);
                let crc_sig: u8 = sig_info[38]
                    | (sig_info[39] << 1)
                    | (sig_info[40] << 2)
                    | (sig_info[41] << 3);
                if crc_calc != crc_sig {
                    return Ok(Pmt::Null);
                }

                let mcs_idx = sig_info[19]
                    | (sig_info[20] << 1)
                    | (sig_info[21] << 2)
                    | (sig_info[22] << 3);
                let mcs = match Mcs::from_mcs_index(mcs_idx) {
                    Some(m) => m,
                    None => return Ok(Pmt::Null),
                };
                let short_gi = sig_info[16] != 0;
                let aggregation = sig_info[24] != 0;
                let mut length: usize = 0;
                for (i, &b) in sig_info[25..=33].iter().enumerate() {
                    length |= (b as usize) << i;
                }
                let traveling_pilots = if is_long {
                    sig_info[37] != 0
                } else {
                    sig_info[36] != 0
                };

                let _ = sc; // keep import used via macro transitively
                ctx.frame_param = Some(FrameParam::with_options(
                    mcs,
                    length,
                    aggregation,
                    traveling_pilots,
                    short_gi,
                    is_long,
                ));

                mio.post("frame", Pmt::Any(Box::new(ctx))).await?;
            }
        }
        Ok(Pmt::Null)
    }
}

impl Default for SigDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for SigDecoder {}

/// Rate-1/2 BCC Viterbi (k=7, G0=171 oct, G1=133 oct). Input: 96 coded bits;
/// Output: 48 decoded info bits. Returns `None` when the trellis is empty.
fn viterbi_decode_48(coded: &[u8; 96]) -> Option<[u8; 48]> {
    const STATES: usize = 64;
    const INF: u32 = u32::MAX / 4;
    let mut m = [[INF; STATES]; 49];
    let mut back = [[0u8; STATES]; 48];
    m[0][0] = 0;
    for t in 0..48 {
        let c0 = coded[2 * t] as u32;
        let c1 = coded[2 * t + 1] as u32;
        for s in 0..STATES {
            if m[t][s] >= INF {
                continue;
            }
            for bit in 0..2u32 {
                let new_state = ((s as u32) << 1 | bit) & 0x3f;
                let reg = ((bit << 6) | (s as u32)) & 0x7f;
                let p0 = ((reg & 0o171).count_ones() & 1) as u32;
                let p1 = ((reg & 0o133).count_ones() & 1) as u32;
                let d = (p0 ^ c0) + (p1 ^ c1);
                let cost = m[t][s].saturating_add(d);
                if cost < m[t + 1][new_state as usize] {
                    m[t + 1][new_state as usize] = cost;
                    back[t][new_state as usize] = ((s as u8) << 1) | (bit as u8);
                }
            }
        }
    }
    let mut best_s = 0usize;
    for s in 0..STATES {
        if m[48][s] < m[48][best_s] {
            best_s = s;
        }
    }
    if m[48][best_s] >= INF {
        return None;
    }
    let mut bits = [0u8; 48];
    let mut s = best_s;
    for t in (0..48).rev() {
        let b = back[t][s];
        bits[t] = b & 1;
        s = (b >> 1) as usize;
    }
    Some(bits)
}
