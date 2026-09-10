//! The v2/v4 per-stage math, refactored into free functions so a streaming
//! state machine can call it on a preamble slice and then on one data symbol
//! at a time. The arithmetic is a direct port — same estimators, same
//! constants, same notebook conventions.

use futuresdr::num_complex::Complex32;

use crate::v5::helpers::{TCP, TS, dft_shift, frac_shift_ramp, linear_slope, ltf_freq, stf_freq};
use crate::{
    DC_INDEX, FFT_SIZE, FrameParam, MAX_PSDU_SIZE, MAX_SYM, Mcs, Modulation, PILOT_PSI, POLARITY,
    ViterbiDecoder, crc4, data_sc_for_symbol, pilot_sc_for_symbol, sc, sig_data_sc,
};

/// Samples of preamble the receiver must hold to finish SIG: 6 symbols plus
/// one FFT window. 1088 samples at 4 MSps ≈ 0.27 ms — versus the 76,448
/// samples (≈19 ms) v2/v4 buffer before they will look at a frame at all.
pub const PREAMBLE_LEN: usize = 6 * TS + FFT_SIZE;

const SIG_INTERLEAVER_PATTERN: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

/// Coarse (Tcp-lag) + fine (Ts-lag) CFO over the two STF symbols.
///
/// v2 applied the coarse rotation to the whole frame before estimating fine.
/// The net rotation is `exp(-j(coarse+fine)n)`, so estimating fine on a
/// locally-corrected copy of the STF gives the same total without touching
/// anything past the preamble — which is what lets the caller stream.
pub fn estimate_cfo(stf: &[Complex32]) -> f32 {
    debug_assert!(stf.len() >= 2 * TS);
    let y = &stf[0..2 * TS];

    let mut acc = Complex32::new(0.0, 0.0);
    for i in TCP..2 * TS {
        acc += y[i] * y[i - TCP].conj();
    }
    let coarse = acc.arg() / (TCP as f32);

    let mut corrected = [Complex32::new(0.0, 0.0); 2 * TS];
    for (n, s) in y.iter().enumerate() {
        corrected[n] = *s * Complex32::from_polar(1.0, -coarse * (n as f32));
    }

    let mut acc = Complex32::new(0.0, 0.0);
    for i in TS..2 * TS {
        acc += corrected[i] * corrected[i - TS].conj();
    }
    let fine = acc.arg() / (TS as f32);

    coarse + fine
}

/// Total symbol-timing offset in samples, from the STF frequency response.
/// `stf` must be CFO-corrected and at least `2*TS + FFT_SIZE` long.
pub fn estimate_sto(stf: &[Complex32]) -> f32 {
    debug_assert!(stf.len() >= 2 * TS + FFT_SIZE);
    let cp_offset = TCP / 2;
    let cp_corr = -(TCP as f32 / 2.0);
    let stf_ref = stf_freq();

    let mut stf_avg = [Complex32::new(0.0, 0.0); FFT_SIZE];
    for j in 0..2 {
        let t0 = cp_offset + j * TS;
        let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
        blk.copy_from_slice(&stf[t0..t0 + FFT_SIZE]);
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

    // Coarse STO: zero-padded DFT of the compact STF response, argmax.
    let n = stf_vals.len() + 1;
    let mut padded = vec![Complex32::new(0.0, 0.0); n];
    let half = stf_vals.len() / 2;
    padded[..half].copy_from_slice(&stf_vals[..half]);
    padded[n - half..].copy_from_slice(&stf_vals[half..]);
    let mut argmax = 0usize;
    let mut best = -1.0f32;
    for kk in 0..n {
        let mut acc = Complex32::new(0.0, 0.0);
        for tt in 0..n {
            let angle = -2.0 * std::f32::consts::PI * (kk as f32) * (tt as f32) / (n as f32);
            acc += padded[tt] * Complex32::from_polar(1.0, angle);
        }
        let m = acc.norm();
        if m > best {
            best = m;
            argmax = kk;
        }
    }
    let mut idx = argmax as i32;
    if idx as usize >= n / 2 {
        idx -= n as i32;
    }
    let coarse_sto = -(idx as f32) / (4.0 * n as f32) * (FFT_SIZE as f32);

    // Fine STO: least-squares slope of per-subcarrier phase.
    let mut phases = vec![0.0f32; stf_vals.len()];
    let mut avg_vec = Complex32::new(0.0, 0.0);
    for i in 0..stf_vals.len() {
        let rot = Complex32::from_polar(
            1.0,
            2.0 * std::f32::consts::PI * coarse_sto * (stf_idxs[i] as f32) / (FFT_SIZE as f32),
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
    let fine_sto = -linear_slope(&xs, &ys) / (2.0 * std::f32::consts::PI) * (FFT_SIZE as f32);

    coarse_sto + fine_sto
}

/// `FFT_SIZE`-bin channel estimate from the two LTF symbols. `preamble` is
/// indexed from the STO-corrected frame origin.
pub fn estimate_channel(preamble: &[Complex32], sto_frac: f32) -> Option<Vec<Complex32>> {
    if preamble.len() < 4 * TS + FFT_SIZE {
        return None;
    }
    let cp_corr_ltf = TCP as f32 / 2.0;
    let ltf_ref = ltf_freq();
    let mut h_est = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
    for j in 0..2 {
        // LTF symbol 0 carries a double guard interval.
        let cp_off_j = if j == 0 { 2 * TCP - TCP / 2 } else { TCP / 2 };
        let t0 = cp_off_j + (j + 2) * TS;
        let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
        blk.copy_from_slice(&preamble[t0..t0 + FFT_SIZE]);
        let fdom = dft_shift(&blk);
        let ramp = frac_shift_ramp(cp_corr_ltf + sto_frac);
        for k in 0..FFT_SIZE {
            h_est[k] += fdom[k] * ramp[k] * ltf_ref[k] * 0.5;
        }
    }
    Some(h_est)
}

/// Equalized SIG subcarriers for both SIG symbols (48 each).
pub fn sig_symbols(
    preamble: &[Complex32],
    h_est: &[Complex32],
    sto_frac: f32,
) -> Option<[[Complex32; 48]; 2]> {
    if preamble.len() < 6 * TS + FFT_SIZE {
        return None;
    }
    let cp_off_sig = TCP / 2;
    let cp_corr_sig = TCP as f32 / 2.0;
    let sig_idxs = sig_data_sc();
    let sig_scale = (52.0f32 / 56.0).sqrt();

    let mut sig_eq = [[Complex32::new(0.0, 0.0); 48]; 2];
    for j in 0..2 {
        let t0 = cp_off_sig + (j + 4) * TS;
        let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
        blk.copy_from_slice(&preamble[t0..t0 + FFT_SIZE]);
        let fdom = dft_shift(&blk);
        let ramp = frac_shift_ramp(cp_corr_sig + sto_frac);
        for (i, &idx) in sig_idxs.iter().enumerate() {
            let h = h_est[idx];
            if h.norm_sqr() > 0.0 {
                sig_eq[j][i] = fdom[idx] * ramp[idx] / h * sig_scale;
            }
        }
    }
    Some(sig_eq)
}

/// Viterbi + CRC-4 over one SIG bit hypothesis.
fn decode_candidate(
    decoder: &mut ViterbiDecoder,
    scratch: &mut [u8; 48],
    sig_bits: &[u8; 96],
    is_long: bool,
) -> Option<FrameParam> {
    let mut deinterleaved = [0u8; 96];
    for sym in 0..2 {
        let offset = sym * 48;
        for i in 0..48 {
            deinterleaved[offset + i] = sig_bits[offset + SIG_INTERLEAVER_PATTERN[i]];
        }
    }
    decoder.decode_raw(&deinterleaved, scratch, 48);

    let sig_info = &*scratch;
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
    if sig_info[1] != 0 {
        return None; // STBC unsupported
    }
    if (sig_info[3] as u8) | ((sig_info[4] as u8) << 1) != 0 {
        return None; // BW must be 2 MHz
    }
    if (sig_info[5] as u8) | ((sig_info[6] as u8) << 1) != 0 {
        return None; // single spatial stream only
    }
    if sig_info[17] != 0 {
        return None; // BCC only
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

    let fp = FrameParam::with_options(
        mcs,
        length,
        aggregation,
        traveling_pilots,
        short_gi,
        is_long,
    );
    if fp.n_symbols() > MAX_SYM || fp.psdu_size() > MAX_PSDU_SIZE {
        return None;
    }
    Some(fp)
}

/// Try the S1G_SHORT and S1G_LONG bit hypotheses, preferring SHORT on a tie —
/// same precedence as v2.
pub fn decode_sig(
    decoder: &mut ViterbiDecoder,
    scratch: &mut [u8; 48],
    sig_eq: &[[Complex32; 48]; 2],
) -> Option<FrameParam> {
    let mut short_bits = [0u8; 96];
    let mut long_bits = [0u8; 96];
    for i in 0..48 {
        short_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
        short_bits[48 + i] = (sig_eq[1][i].im > 0.0) as u8;
        long_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
        long_bits[48 + i] = (sig_eq[1][i].re > 0.0) as u8;
    }
    decode_candidate(decoder, scratch, &short_bits, false)
        .or_else(|| decode_candidate(decoder, scratch, &long_bits, true))
}

/// Running pilot-phase state carried across the data symbols of one frame.
///
/// This is what makes the whole thing streamable: v2's `demod_frame` walks
/// every data symbol in one pass carrying `accum_alpha`/`accum_beta`, and
/// that carry is the only inter-symbol coupling in the data path.
pub struct PilotTracker {
    accum_alpha: f32,
    accum_beta: f32,
}

impl PilotTracker {
    pub fn new() -> Self {
        Self {
            accum_alpha: 0.0,
            accum_beta: 0.0,
        }
    }

    /// Demodulate one data symbol. `block` is the `FFT_SIZE`-sample window for
    /// this symbol (CP already skipped); `nsym` is its index within the frame.
    /// Writes `N_DATA_SC` demapped values into `out`.
    #[allow(clippy::too_many_arguments)]
    pub fn demod_symbol(
        &mut self,
        block: &[Complex32],
        h_est: &[Complex32],
        sto_frac: f32,
        nsym: usize,
        short_gi: bool,
        traveling_pilots: bool,
        modulation: Modulation,
        out: &mut [u8],
        syms_out: &mut Vec<Complex32>,
    ) {
        let fdom = dft_shift(block);
        let cp_corr = if short_gi {
            TCP as f32 / 4.0
        } else {
            TCP as f32 / 2.0
        };
        let ramp = frac_shift_ramp(cp_corr + sto_frac);

        let mut sym = [Complex32::new(0.0, 0.0); FFT_SIZE];
        for k in 0..FFT_SIZE {
            sym[k] = fdom[k] * ramp[k];
            if h_est[k].norm_sqr() > 0.0 {
                sym[k] /= h_est[k];
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
            let prev = self.accum_alpha * (xs[k] - x_mean) + self.accum_beta;
            residual += w[k] * Complex32::from_polar(1.0, -prev);
        }
        let resid_beta = residual.arg();
        let resid_rot = Complex32::from_polar(1.0, -resid_beta);
        let mut num = 0.0f32;
        let mut den = 0.0f32;
        for k in 0..4 {
            let prev = self.accum_alpha * (xs[k] - x_mean) + self.accum_beta;
            let wr = w[k] * Complex32::from_polar(1.0, -prev) * resid_rot;
            let dx = xs[k] - x_mean;
            num += dx * wr.im;
            den += dx * dx;
        }
        let resid_alpha = if den > 0.0 { num / den } else { 0.0 };
        self.accum_alpha += resid_alpha;
        self.accum_beta += resid_beta;

        for k in 0..FFT_SIZE {
            let off = (k as f32) - (DC_INDEX as f32);
            sym[k] *= Complex32::from_polar(
                1.0,
                -(self.accum_alpha * (off - x_mean) + self.accum_beta),
            );
        }

        for (o, &idx) in data_sc_for_symbol(nsym, traveling_pilots).iter().enumerate() {
            out[o] = modulation.demap(&sym[idx]);
            syms_out.push(sym[idx]);
        }
    }
}

impl Default for PilotTracker {
    fn default() -> Self {
        Self::new()
    }
}
