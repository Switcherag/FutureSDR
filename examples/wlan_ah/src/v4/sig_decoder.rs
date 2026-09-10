//! Demodulates the two SIG OFDM symbols, runs inline BCC Viterbi (rate-1/2,
//! G0=171, G1=133) on the 96 coded bits, checks CRC-4, and parses the SIG
//! field into a `FrameParam`. Emits the updated context downstream only when
//! the CRC passes.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

use crate::v4::ctx::FrameCtx;
use crate::v4::helpers::{TCP, TS, dft_shift, frac_shift_ramp};
use crate::{FFT_SIZE, FrameParam, MAX_PSDU_SIZE, MAX_SYM, Mcs, ViterbiDecoder, crc4, sig_data_sc};

const SIG_INTERLEAVER_PATTERN: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13,
    16, 19, 22, 25, 28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26,
    29, 32, 35, 38, 41, 44, 47,
];

#[derive(Block)]
#[message_inputs(frame)]
#[message_outputs(frame, preamble_symbols, preamble_symbols2)]
pub struct SigDecoder {
    debug_print: bool,
    decoder: ViterbiDecoder,
    decoded_bits: [u8; 48],
}

impl SigDecoder {
    pub fn new() -> Self {
        Self::new_with_debug_print(false)
    }

    pub fn new_with_debug_print(debug_print: bool) -> Self {
        Self {
            debug_print,
            decoder: ViterbiDecoder::new(),
            decoded_bits: [0; 48],
        }
    }

    fn decode_candidate(&mut self, sig_bits: &[u8; 96], is_long: bool) -> Option<FrameParam> {
        let mut deinterleaved = [0u8; 96];
        for sym in 0..2 {
            let offset = sym * 48;
            for i in 0..48 {
                deinterleaved[offset + i] = sig_bits[offset + SIG_INTERLEAVER_PATTERN[i]];
            }
        }

        self.decoder
            .decode_raw(&deinterleaved, &mut self.decoded_bits, 48);

        let sig_info = &self.decoded_bits;
        let crc_calc = crc4(&sig_info[0..38]);
        let mut crc_sig = 0u8;
        for i in 0..4 {
            if sig_info[38 + i] > 0 {
                crc_sig |= 1 << (3 - i);
            }
        }
        if crc_calc != crc_sig {
            return None;
        }

        let stbc = sig_info[1];
        if stbc != 0 {
            return None;
        }
        let bw = (sig_info[3] as u8) | ((sig_info[4] as u8) << 1);
        if bw != 0 {
            return None;
        }
        let nsts = (sig_info[5] as u8) | ((sig_info[6] as u8) << 1);
        if nsts != 0 {
            return None;
        }
        let coding = sig_info[17];
        if coding != 0 {
            return None;
        }

        let mcs_idx = (sig_info[19] as u8)
            | ((sig_info[20] as u8) << 1)
            | ((sig_info[21] as u8) << 2)
            | ((sig_info[22] as u8) << 3);
        let mcs = Mcs::from_mcs_index(mcs_idx)?;
        let short_gi = sig_info[16] > 0;
        let aggregation = sig_info[24] > 0;
        let mut length: usize = 0;
        for (i, &b) in sig_info[25..=33].iter().enumerate() {
            length |= (b as usize) << i;
        }
        let traveling_pilots = if is_long {
            sig_info[37] > 0
        } else {
            sig_info[36] > 0
        };

        let frame_param = FrameParam::with_options(
            mcs,
            length,
            aggregation,
            traveling_pilots,
            short_gi,
            is_long,
        );
        if frame_param.n_symbols() > MAX_SYM || frame_param.psdu_size() > MAX_PSDU_SIZE {
            return None;
        }

        Some(frame_param)
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
            mio.post("preamble_symbols", Pmt::Finished).await?;
            mio.post("preamble_symbols2", Pmt::Finished).await?;
            io.finished = true;
            return Ok(Pmt::Null);
        }
        // Take ownership of the frame instead of cloning it: v2 deep-copied
        // `FrameCtx::samples` (the whole buffered frame) here, once per stage.
        if let Pmt::Any(a) = p {
            if let Some(mut ctx) = a.take::<FrameCtx>() {
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
                let sig_scale = (52.0f32 / 56.0).sqrt();
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

                let mut short_bits = [0u8; 96];
                let mut long_bits = [0u8; 96];
                for i in 0..48 {
                    short_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
                    short_bits[48 + i] = (sig_eq[1][i].im > 0.0) as u8;

                    long_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
                    long_bits[48 + i] = (sig_eq[1][i].re > 0.0) as u8;
                }

                let short_frame = self.decode_candidate(&short_bits, false);
                let long_frame = self.decode_candidate(&long_bits, true);
                let frame_param = match (short_frame, long_frame) {
                    (Some(short), None) => short,
                    (None, Some(long)) => long,
                    (Some(short), Some(_long)) => short,
                    (None, None) => {
                        if self.debug_print {
                            let imag_pow: f32 = sig_eq[1].iter().map(|z| z.im * z.im).sum();
                            let real_pow: f32 = sig_eq[1].iter().map(|z| z.re * z.re).sum();
                            info!(
                                "[v4.sig] decode failed: sym2 imag_pow={:.3} real_pow={:.3}",
                                imag_pow,
                                real_pow
                            );
                        }
                        return Ok(Pmt::Null);
                    }
                };

                if self.debug_print {
                    info!(
                        "[v4.sig] decoded: mcs={:?} n_sym={} psdu={} long={} tp={} sgi={} agg={}",
                        frame_param.mcs(),
                        frame_param.n_symbols(),
                        frame_param.psdu_size(),
                        frame_param.is_long,
                        frame_param.traveling_pilots,
                        frame_param.short_gi,
                        frame_param.aggregation
                    );
                }

                ctx.frame_param = Some(frame_param);

                mio.post("frame", Pmt::Any(ctx)).await?;
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
