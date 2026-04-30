//! All-in-one PPDU decoder, line-by-line port of the analysis notebook's
//! `decode_ppdu` function. Buffers all input samples, runs detection +
//! decoding offline once the source finishes, and emits each decoded PSDU
//! on the `rx_frames` message port.
//!
//! Skipped vs the notebook (TODO):
//!   * S1G_LONG (D-STF / D-LTF1 / SIG-B) — only S1G_SHORT (the common case)
//!   * A-MPDU aggregation
//!   * Traveling pilots
//!   * Soft-decision Viterbi (we use the existing hard-decision decoder)

use std::sync::Arc;

use futuresdr::prelude::*;
use rustfft::{Fft, FftPlanner};

use crate::{
    crc4, sc, sig_data_sc, FrameParam, LTF_FREQ, Mcs, ViterbiDecoder, CP_LEN, DC_INDEX, FFT_SIZE,
    MAX_ENCODED_BITS, MAX_PSDU_SIZE, MAX_SYM, N_DATA_SC, N_SIG_DATA_SC, PILOT_OFFSETS, PILOT_PSI,
    POLARITY, SYMBOL_LEN,
};

// ── notebook constants @ 4 MSps ──────────────────────────────────────────
const TU: usize = FFT_SIZE; // 128
const TCP: usize = CP_LEN; // 32
const TS: usize = SYMBOL_LEN; // 160

// Detection: same recipe as `decode_ppdu`'s caller (notebook detection cell).
const DETECT_WIN: usize = 6 * TS; // 960
const LOCAL_MAX_R: usize = 2 * TS; // 320
const STF_DELAY: usize = TU / 4; // 32
const STF_CORR_WIN: usize = 2 * TS - STF_DELAY; // 288
const STF_POWER_WIN: usize = TS / 2; // 80
// Notebook threshold = 30 dB on mean-of-power metric. In sum-of-power units we
// use M ≥ 10**(30/10) / N_pow = 12.5 — same as plots/render.py.
const DETECT_THRESHOLD: f32 = 12.5;

const SIG_INTERLEAVER_PATTERN: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

// Notebook stf_syms (offsets -26..+26, length 53). 12 non-zero values at
// every 4th subcarrier, with a gap at DC.
fn stf_syms() -> [Complex32; 53] {
    let inv_sqrt2 = 1.0f32 / 2.0f32.sqrt();
    let p = Complex32::new(inv_sqrt2, inv_sqrt2);
    let n = -p;
    let z = Complex32::new(0.0, 0.0);
    let mut a = [z; 53];
    a[2] = p;
    a[6] = n;
    a[10] = p;
    a[14] = n;
    a[18] = n;
    a[22] = p;
    a[30] = n;
    a[34] = n;
    a[38] = p;
    a[42] = p;
    a[46] = p;
    a[50] = p;
    a
}

// ── Streaming Schmidl-Cox detection (rust-streaming, sum-based) ─────────
// Returns the indices in `x` corresponding to detected STF starts. The
// returned indices are aligned to the *first* sample of the correlation
// window (matches the notebook's `max_idx[detections]`).
fn detect_ppdus(x: &[Complex32]) -> Vec<usize> {
    let n = x.len();
    if n < STF_CORR_WIN + STF_DELAY + STF_POWER_WIN {
        return Vec::new();
    }

    // Compute streaming metric M[n] = |Σ_{k=n-287..n} x[k]·conj(x[k-32])| / Σ|x|²
    // by tracking running sums.
    let mut metric = vec![0.0f32; n];

    // Power running sum (window = STF_POWER_WIN, sliding).
    let mut pow_sum = 0.0f32;
    for i in 0..STF_POWER_WIN.min(n) {
        pow_sum += x[i].norm_sqr();
    }
    // Correlation running sum (window = STF_CORR_WIN, lag = STF_DELAY).
    // Valid only for indices ≥ STF_DELAY + STF_CORR_WIN - 1.
    let mut corr_sum = Complex32::new(0.0, 0.0);
    let lag = STF_DELAY;
    let cw = STF_CORR_WIN;
    if n > lag + cw {
        for k in 0..cw {
            corr_sum += x[k + lag] * x[k].conj();
        }
    }

    // Now slide. At each output index i (∈ [lag+cw-1, n-1]) we have:
    //   pow_sum  = Σ_{k=i-Ts/2+1..i} |x[k]|²    (after sliding the power window)
    //   corr_sum = Σ_{k=i-cw+1..i}   x[k]·conj(x[k-lag])
    // Slide both cursors in lockstep.

    // Re-initialise correlation/power so that sums end at the same index.
    // We'll iterate i from i_start to n-1, keeping sums "ending at i".
    let i_start = lag + cw - 1;
    if n <= i_start {
        return Vec::new();
    }
    // Re-seed corr_sum to be "ending at i_start"
    corr_sum = Complex32::new(0.0, 0.0);
    for k in 0..cw {
        let j = i_start - (cw - 1) + k; // covers j = i_start - cw + 1 .. i_start
        corr_sum += x[j] * x[j - lag].conj();
    }
    // Re-seed pow_sum to be "ending at i_start"
    pow_sum = 0.0;
    let pw = STF_POWER_WIN;
    for k in 0..pw {
        if i_start >= pw - 1 {
            pow_sum += x[i_start - (pw - 1) + k].norm_sqr();
        }
    }

    for i in i_start..n {
        if pow_sum > 1e-12 {
            metric[i] = corr_sum.norm() / pow_sum;
        }
        // Slide: drop x[i-cw+1]·conj(x[i-cw+1-lag]); add x[i+1]·conj(x[i+1-lag])
        if i + 1 < n {
            let drop_corr = x[i - cw + 1] * x[i - cw + 1 - lag].conj();
            let add_corr = x[i + 1] * x[i + 1 - lag].conj();
            corr_sum = corr_sum - drop_corr + add_corr;

            let drop_pow = x[i - pw + 1].norm_sqr();
            let add_pow = x[i + 1].norm_sqr();
            pow_sum = pow_sum - drop_pow + add_pow;
        }
    }

    // Block argmax + local-max + threshold (notebook detection cell).
    let n_blocks = n / DETECT_WIN;
    let mut detections = Vec::new();
    for j in 0..n_blocks {
        let lo = j * DETECT_WIN;
        let hi = lo + DETECT_WIN;
        let mut max_val = f32::NEG_INFINITY;
        let mut max_pos = lo;
        for k in lo..hi {
            if metric[k] > max_val {
                max_val = metric[k];
                max_pos = k;
            }
        }
        if max_val < DETECT_THRESHOLD {
            continue;
        }
        // Local-max check over ±2·Ts.
        let lm_lo = max_pos.saturating_sub(LOCAL_MAX_R);
        let lm_hi = (max_pos + LOCAL_MAX_R).min(n);
        let mut lm_max = f32::NEG_INFINITY;
        for k in lm_lo..lm_hi {
            if metric[k] > lm_max {
                lm_max = metric[k];
            }
        }
        if max_val < lm_max {
            continue;
        }
        // Notebook converts the convolution-domain index to the start of the
        // correlation window in x:  start_idx = i_metric - (CORR_WIN - 1)
        // (the metric "ending at i" sums x[i - cw + 1 ..= i]).
        // Plus the rust delay block adds another `lag` of bookkeeping → so we
        // step back by (CORR_WIN - 1) + STF_DELAY.
        let start_idx = max_pos.saturating_sub(STF_CORR_WIN - 1 + STF_DELAY);
        detections.push(start_idx);
    }
    detections
}

// ── Helpers ─────────────────────────────────────────────────────────────

#[inline]
fn fftshift(buf: &mut [Complex32]) {
    let n = buf.len();
    let half = n / 2;
    for i in 0..half {
        buf.swap(i, i + half);
    }
}

#[inline]
fn fftshift_freqs(n: usize) -> Vec<f32> {
    // np.fft.fftshift(np.fft.fftfreq(n)) at unit sample spacing.
    let mut f = Vec::with_capacity(n);
    for k in 0..n {
        let ki = if k < n / 2 { k as i32 } else { k as i32 - n as i32 };
        f.push(ki as f32 / n as f32);
    }
    let mut shifted = vec![0.0; n];
    let half = n / 2;
    for i in 0..n {
        shifted[i] = f[(i + half) % n];
    }
    shifted
}

/// Linear regression slope (returns `m` of `y ≈ m·x + b`).
fn polyfit_slope(xs: &[f32], ys: &[f32]) -> f32 {
    let n = xs.len() as f32;
    let mean_x = xs.iter().sum::<f32>() / n;
    let mean_y = ys.iter().sum::<f32>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..xs.len() {
        let dx = xs[i] - mean_x;
        num += dx * (ys[i] - mean_y);
        den += dx * dx;
    }
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

// ── The block ───────────────────────────────────────────────────────────

#[derive(Block)]
#[message_outputs(rx_frames)]
pub struct PpduProcessor<I = DefaultCpuReader<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
{
    #[input]
    input: I,
    samples: Vec<Complex32>,
    flushed: bool,
    fft128: Arc<dyn Fft<f32>>,
    viterbi: ViterbiDecoder,
    rx_bits: Vec<u8>,
    deint_bits: Vec<u8>,
    decoded_bits: Vec<u8>,
    out_bytes: Vec<u8>,
}

impl<I> PpduProcessor<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        Self {
            input: I::default(),
            samples: Vec::new(),
            flushed: false,
            fft128: planner.plan_fft_forward(TU),
            viterbi: ViterbiDecoder::new(),
            rx_bits: vec![0u8; MAX_ENCODED_BITS],
            deint_bits: vec![0u8; MAX_ENCODED_BITS],
            decoded_bits: vec![0u8; MAX_ENCODED_BITS],
            out_bytes: vec![0u8; MAX_PSDU_SIZE + 2],
        }
    }
}

impl<I> Default for PpduProcessor<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I> Kernel for PpduProcessor<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let in_slice = self.input.slice();
        let n = in_slice.len();
        if n > 0 {
            self.samples.extend_from_slice(in_slice);
            self.input.consume(n);
        }
        if self.input.finished() && !self.flushed {
            self.flushed = true;
            self.flush(mio).await?;
            io.finished = true;
        }
        Ok(())
    }
}

impl<I> PpduProcessor<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    async fn flush(&mut self, mio: &mut MessageOutputs) -> Result<()> {
        let detections = detect_ppdus(&self.samples);
        info!(
            "v3: input has {} samples, {} detections (threshold {:.2})",
            self.samples.len(),
            detections.len(),
            DETECT_THRESHOLD
        );
        for (n, &start_idx) in detections.iter().enumerate() {
            match self.decode_ppdu(start_idx) {
                Some(psdu) => {
                    info!(
                        "v3: PPDU {} @ sample {} → {} bytes",
                        n,
                        start_idx,
                        psdu.len()
                    );
                    mio.post("rx_frames", Pmt::Blob(psdu)).await?;
                }
                None => {
                    info!("v3: PPDU {} @ sample {} — decode failed", n, start_idx);
                }
            }
        }
        mio.post("rx_frames", Pmt::Finished).await?;
        Ok(())
    }

    /// Line-by-line port of the notebook's `decode_ppdu`.
    fn decode_ppdu(&mut self, start_idx_in: usize) -> Option<Vec<u8>> {
        let _dbg_idx = start_idx_in;
        macro_rules! dbg_fail {
            ($($t:tt)*) => {{
                info!("v3:   @ {} fail: {}", _dbg_idx, format!($($t)*));
                return None;
            }};
        }
        let x = &self.samples;
        let n_x = x.len();

        // Need at least 2·Ts after start_idx for the STF window
        if start_idx_in + 2 * TS > n_x {
            return None;
        }
        let mut start_idx = start_idx_in;

        // ── 1. Coarse CFO from STF Tcp-lag inner product ────────────────
        let mut stf_corr = Complex32::new(0.0, 0.0);
        for k in 0..(2 * TS - TCP) {
            stf_corr += x[start_idx + TCP + k] * x[start_idx + k].conj();
        }
        let coarse_cfo = stf_corr.arg() / TCP as f32;

        // ── 2. Fine CFO from STF Ts-lag inner product (after coarse correction) ─
        let mut stf_corr2 = Complex32::new(0.0, 0.0);
        for k in 0..TS {
            let i0 = start_idx + k;
            let i1 = start_idx + k + TS;
            let y0 =
                x[i0] * Complex32::from_polar(1.0, -coarse_cfo * (i0 - start_idx) as f32);
            let y1 =
                x[i1] * Complex32::from_polar(1.0, -coarse_cfo * (i1 - start_idx) as f32);
            stf_corr2 += y1 * y0.conj();
        }
        let fine_cfo = stf_corr2.arg() / TS as f32;
        let cfo = coarse_cfo + fine_cfo;

        // Reusable per-symbol time-domain buffer (Tu samples).
        let mut buf = vec![Complex32::new(0.0, 0.0); TU];
        let _ = cfo; // (used inline below)

        // ── 3. STF demod (with cfo correction) → STO via FFT-argmax ─────
        // Demodulate two STF symbols at start_idx + cp_offset + j·Ts, length Tu
        let cp_offset_stf: i32 = (TCP / 2) as i32;
        let cp_corr_stf: f32 = -((TCP / 2) as f32);
        let mut stf_spec = vec![[Complex32::new(0.0, 0.0); 53]; 2]; // -26..+26 each
        let mut stf_avg_with_dc = vec![Complex32::new(0.0, 0.0); 13 + 1]; // 12 non-zero + DC pad
        let stfs = stf_syms();

        let mut stf_eq_avg = [Complex32::new(0.0, 0.0); 12]; // post-equalization
        // (intermediate; we don't actually need it for STO computation below)
        let _ = stf_eq_avg;

        // Per-symbol STF demod
        for j in 0..2 {
            let base = start_idx as i32 + cp_offset_stf + (j as i32) * TS as i32;
            if base < 0 || (base as usize + TU) > n_x {
                return None;
            }
            for k in 0..TU {
                let t = (base as usize) + k;
                let cfo_arg = cfo * (t as i64 - start_idx as i64) as f32;
                buf[k] = x[t] * Complex32::from_polar(1.0, -cfo_arg);
            }
            self.fft128.process(&mut buf);
            // fftshift in place
            fftshift(&mut buf);
            // post-FFT phase rotation: exp(j 2π cp_corr fftfreq(Tu)) (fftshifted)
            let freqs = fftshift_freqs(TU); // -0.5..0.5
            for k in 0..TU {
                let phase = 2.0 * std::f32::consts::PI * cp_corr_stf * freqs[k];
                buf[k] *= Complex32::from_polar(1.0, phase);
            }
            // Take subcarriers -26..+26 (53 values: indices DC-26 .. DC+26)
            for s in 0..53 {
                let idx = (DC_INDEX as i32 - 26 + s as i32) as usize;
                stf_spec[j][s] = buf[idx] * stfs[s].conj();
            }
        }

        // Sum over the two STF symbols, then keep only non-zero stf subcarriers (12 of them).
        let mut nz_indices_53 = Vec::with_capacity(12);
        for s in 0..53 {
            if stfs[s].norm_sqr() > 0.0 {
                nz_indices_53.push(s);
            }
        }
        let mut stf_avg = vec![Complex32::new(0.0, 0.0); nz_indices_53.len()];
        for (k, &s) in nz_indices_53.iter().enumerate() {
            stf_avg[k] = stf_spec[0][s] + stf_spec[1][s];
        }
        // Pad with zero at DC (size = nz + 1)
        for v in stf_avg_with_dc.iter_mut() {
            *v = Complex32::new(0.0, 0.0);
        }
        let half = stf_avg.len() / 2;
        let dc_len = stf_avg_with_dc.len();
        for k in 0..half {
            stf_avg_with_dc[k] = stf_avg[k];
        }
        for k in 0..half {
            stf_avg_with_dc[dc_len - half + k] = stf_avg[half + k];
        }
        // FFT the padded STF spectrum to find coarse STO
        let mut planner = FftPlanner::<f32>::new();
        let stf_fft_size = stf_avg_with_dc.len(); // 13
        let stf_fft = planner.plan_fft_forward(stf_fft_size);
        stf_fft.process(&mut stf_avg_with_dc);
        let mut stf_fft_max = 0.0f32;
        let mut stf_fft_max_idx: i32 = 0;
        for (k, v) in stf_avg_with_dc.iter().enumerate() {
            let m = v.norm();
            if m > stf_fft_max {
                stf_fft_max = m;
                stf_fft_max_idx = k as i32;
            }
        }
        if stf_fft_max_idx >= (stf_fft_size as i32) / 2 {
            stf_fft_max_idx -= stf_fft_size as i32;
        }
        let coarse_sto =
            -(stf_fft_max_idx as f32) / (4.0 * stf_fft_size as f32) * TU as f32;

        // Fine STO: apply coarse_sto, then linear phase fit on stf_avg
        let stf_subc_idxs: Vec<i32> = (-26..=26)
            .filter(|&i| {
                let s = (i + 26) as usize;
                stfs[s].norm_sqr() > 0.0
            })
            .collect();
        let mut stf_without_coarse = Vec::with_capacity(stf_avg.len());
        for (k, &i) in stf_subc_idxs.iter().enumerate() {
            let phase = 2.0 * std::f32::consts::PI * coarse_sto * (i as f32) / TU as f32;
            // Note: stf_avg was already overwritten by FFT above. Recompute:
            let s = (i + 26) as usize;
            let val = stf_spec[0][s] + stf_spec[1][s];
            stf_without_coarse.push(val * Complex32::from_polar(1.0, phase));
            let _ = k;
        }
        // average phase
        let avg_c: Complex32 = stf_without_coarse.iter().copied().sum();
        let avg_phase = avg_c.arg();
        let xs: Vec<f32> = stf_subc_idxs.iter().map(|&i| i as f32).collect();
        let ys: Vec<f32> = stf_without_coarse
            .iter()
            .map(|c| (c * Complex32::from_polar(1.0, -avg_phase)).arg())
            .collect();
        let slope = polyfit_slope(&xs, &ys);
        let fine_sto = -slope / (2.0 * std::f32::consts::PI) * TU as f32;
        let mut sto = coarse_sto + fine_sto;

        // Adjust start_idx by integer part of sto
        let sto_round = sto.round() as i32;
        let new_start = start_idx as i32 + sto_round;
        if new_start < 0 || (new_start as usize) + 6 * TS > n_x {
            return None;
        }
        start_idx = new_start as usize;
        sto -= sto_round as f32;

        // ── 4. LTF1 demod → h_est ──────────────────────────────────────
        let cp_corr_ltf = (TCP / 2) as f32;
        let mut ltf1_spec = vec![[Complex32::new(0.0, 0.0); 56]; 2];
        for j in 0..2 {
            let cp_offset_ltf: i32 = if j == 0 {
                (2 * TCP - TCP / 2) as i32
            } else {
                (TCP / 2) as i32
            };
            let base = start_idx as i32 + cp_offset_ltf + ((j + 2) as i32) * TS as i32;
            if base < 0 || (base as usize + TU) > n_x {
                return None;
            }
            for k in 0..TU {
                let t = (base as usize) + k;
                let cfo_arg = cfo * (t as i64 - start_idx as i64) as f32;
                buf[k] = x[t] * Complex32::from_polar(1.0, -cfo_arg);
            }
            self.fft128.process(&mut buf);
            fftshift(&mut buf);
            let freqs = fftshift_freqs(TU);
            for k in 0..TU {
                let phase = 2.0 * std::f32::consts::PI * (cp_corr_ltf + sto) * freqs[k];
                buf[k] *= Complex32::from_polar(1.0, phase);
            }
            // active subcarriers -28..+28 excl 0  (56 of them) × ltf
            let mut k_active = 0usize;
            for off in -28i32..=28 {
                if off == 0 {
                    continue;
                }
                let idx = sc(off);
                ltf1_spec[j][k_active] = buf[idx] * Complex32::new(LTF_FREQ[k_active], 0.0);
                k_active += 1;
            }
        }
        // Average LTF1 sym1 + sym2 → h_est at active subcarriers
        let mut h_est = vec![Complex32::new(0.0, 0.0); TU];
        let mut k_active = 0usize;
        for off in -28i32..=28 {
            if off == 0 {
                continue;
            }
            let idx = sc(off);
            h_est[idx] = (ltf1_spec[0][k_active] + ltf1_spec[1][k_active]) * 0.5;
            k_active += 1;
        }

        // ── 5. SIG demod ───────────────────────────────────────────────
        let cp_offset_sig: i32 = (TCP / 2) as i32;
        let cp_corr_sig: f32 = (TCP / 2) as f32;
        let mut sig_spec = vec![Vec::<Complex32>::new(); 2];
        for j in 0..2 {
            let base = start_idx as i32 + cp_offset_sig + ((j + 4) as i32) * TS as i32;
            if base < 0 || (base as usize + TU) > n_x {
                return None;
            }
            for k in 0..TU {
                let t = (base as usize) + k;
                let cfo_arg = cfo * (t as i64 - start_idx as i64) as f32;
                buf[k] = x[t] * Complex32::from_polar(1.0, -cfo_arg);
            }
            self.fft128.process(&mut buf);
            fftshift(&mut buf);
            let freqs = fftshift_freqs(TU);
            for k in 0..TU {
                let phase = 2.0 * std::f32::consts::PI * (cp_corr_sig + sto) * freqs[k];
                buf[k] *= Complex32::from_polar(1.0, phase);
            }
            sig_spec[j] = buf.clone();
        }

        // SIG bits: 2 × 48 = 96 coded bits, decision = imag > 0  (BPSK rotated)
        // Notebook scaling = sqrt(sig_subc.size / 56) where sig_subc.size = 52
        // (subcarriers ±1..±26 incl pilots, excl DC). We have 48 data subc + 4 pilots = 52.
        let scale = (52.0f32 / 56.0).sqrt();
        let sig_data = sig_data_sc();
        let mut sig_bits = [0u8; 2 * N_SIG_DATA_SC];
        let mut first_eq = [Complex32::new(0.0, 0.0); 4];
        for sym in 0..2 {
            for (i, &sub_idx) in sig_data.iter().enumerate() {
                let eq = sig_spec[sym][sub_idx] / h_est[sub_idx] * Complex32::new(scale, 0.0);
                if sym == 0 && i < 4 {
                    first_eq[i] = eq;
                }
                // BPSK rotated: signal on imaginary axis (assume S1G_SHORT)
                sig_bits[sym * N_SIG_DATA_SC + i] = if eq.im > 0.0 { 1 } else { 0 };
            }
        }
        info!(
            "v3:   @ {} sig sym1[0..4] = ({:.3}{:+.3}j) ({:.3}{:+.3}j) ({:.3}{:+.3}j) ({:.3}{:+.3}j)",
            _dbg_idx,
            first_eq[0].re, first_eq[0].im,
            first_eq[1].re, first_eq[1].im,
            first_eq[2].re, first_eq[2].im,
            first_eq[3].re, first_eq[3].im,
        );

        // Deinterleave (per-symbol pattern), Viterbi decode (rate 1/2), CRC-4
        let mut deinterleaved = vec![0u8; 96];
        for sym in 0..2 {
            for i in 0..N_SIG_DATA_SC {
                deinterleaved[sym * N_SIG_DATA_SC + i] =
                    sig_bits[sym * N_SIG_DATA_SC + SIG_INTERLEAVER_PATTERN[i]];
            }
        }
        let mut decoded_sig = [0u8; 48];
        self.viterbi.decode_raw(&deinterleaved, &mut decoded_sig, 48);
        let crc_calc = crc4(&decoded_sig[0..38]);
        let mut crc_sig: u8 = 0;
        for i in 0..4 {
            if decoded_sig[38 + i] > 0 {
                crc_sig |= 1 << (3 - i);
            }
        }
        info!(
            "v3:   @ {} cfo(coarse={:.6}, fine={:.6}, total={:.6})  sto({:.3} samples) crc4(calc={:#x}, got={:#x})",
            _dbg_idx, coarse_cfo, fine_cfo, cfo, sto + sto_round as f32, crc_calc, crc_sig
        );
        if crc_calc != crc_sig {
            dbg_fail!("SIG CRC-4 mismatch");
        }

        // Parse SIG (S1G_SHORT layout)
        let stbc = decoded_sig[1];
        if stbc != 0 { dbg_fail!("STBC = {}", stbc); }
        let bw = (decoded_sig[3] as u8) | ((decoded_sig[4] as u8) << 1);
        if bw != 0 { dbg_fail!("BW = {}", bw); }
        let nsts = (decoded_sig[5] as u8) | ((decoded_sig[6] as u8) << 1);
        if nsts != 0 { dbg_fail!("NSTS = {}", nsts); }
        let short_gi = decoded_sig[16] > 0;
        let coding = decoded_sig[17];
        if coding != 0 { dbg_fail!("LDPC coding"); }
        let mcs_idx = (decoded_sig[19] as u8)
            | ((decoded_sig[20] as u8) << 1)
            | ((decoded_sig[21] as u8) << 2)
            | ((decoded_sig[22] as u8) << 3);
        let aggregation = decoded_sig[24] > 0;
        let mut length: usize = 0;
        for i in 0..9 {
            if decoded_sig[25 + i] > 0 {
                length |= 1 << i;
            }
        }
        let traveling_pilots = decoded_sig[36] > 0;
        if traveling_pilots {
            dbg_fail!("traveling pilots not supported");
        }
        let mcs = match Mcs::from_mcs_index(mcs_idx) {
            Some(m) => m,
            None => dbg_fail!("unsupported MCS {}", mcs_idx),
        };
        info!(
            "v3:   @ {} SIG ok: MCS={:?} len={} agg={} short_gi={}",
            _dbg_idx, mcs, length, aggregation, short_gi
        );
        let frame = FrameParam::with_options(mcs, length, aggregation, false, short_gi, false);
        if frame.n_symbols() > MAX_SYM || frame.psdu_size() > MAX_PSDU_SIZE {
            dbg_fail!("frame too large: n_sym={} psdu={}", frame.n_symbols(), frame.psdu_size());
        }

        // ── 6. Data symbols demod (no extra symbols, S1G_SHORT) ────────
        let n_sym = frame.n_symbols();
        let mut syms = vec![vec![Complex32::new(0.0, 0.0); TU]; n_sym];
        let mut t_idx = start_idx as i64 + 6 * TS as i64;
        for j in 0..n_sym {
            let gi = if short_gi && j >= 1 { TCP / 2 } else { TCP };
            let cp_offset = gi / 2;
            let base = t_idx + cp_offset as i64;
            if base < 0 || (base as usize + TU) > n_x {
                return None;
            }
            for k in 0..TU {
                let t = (base as usize) + k;
                let cfo_arg = cfo * (t as i64 - start_idx as i64) as f32;
                buf[k] = x[t] * Complex32::from_polar(1.0, -cfo_arg);
            }
            self.fft128.process(&mut buf);
            fftshift(&mut buf);
            // post-FFT phase shift: depends on short_gi vs not
            let freqs = fftshift_freqs(TU);
            let cp_phase_factor = if short_gi && j >= 1 {
                (TCP / 4) as f32
            } else {
                (TCP / 2) as f32
            };
            for k in 0..TU {
                let phase =
                    2.0 * std::f32::consts::PI * (cp_phase_factor + sto) * freqs[k];
                buf[k] *= Complex32::from_polar(1.0, phase);
            }
            syms[j].copy_from_slice(&buf);
            t_idx += (TU + gi) as i64;
        }

        // Equalise each symbol via h_est, then per-symbol pilot polyfit
        // (cumulative slope+intercept like the existing FrameEqualizer).
        let pilot_indices = [
            sc(PILOT_OFFSETS[0]),
            sc(PILOT_OFFSETS[1]),
            sc(PILOT_OFFSETS[2]),
            sc(PILOT_OFFSETS[3]),
        ];
        let pilot_xs: [f32; 4] = [
            (pilot_indices[0] as f32) - DC_INDEX as f32,
            (pilot_indices[1] as f32) - DC_INDEX as f32,
            (pilot_indices[2] as f32) - DC_INDEX as f32,
            (pilot_indices[3] as f32) - DC_INDEX as f32,
        ];
        let x_mean = (pilot_xs[0] + pilot_xs[1] + pilot_xs[2] + pilot_xs[3]) * 0.25;
        let mut accum_alpha = 0.0f32;
        let mut accum_beta = 0.0f32;

        // 52 data subcarriers (offsets, excluding pilots and DC)
        let mut data_off: Vec<i32> = Vec::with_capacity(N_DATA_SC);
        for off in -28i32..=28 {
            if off == 0 {
                continue;
            }
            if PILOT_OFFSETS.contains(&off) {
                continue;
            }
            data_off.push(off);
        }

        // Hard-decision demap of every data symbol → bits packed as the
        // existing decoder expects (one byte per subcarrier with bits 0..bpsc-1)
        let bpsc = mcs.modulation().n_bpsc();
        let n_data_bits = n_sym * N_DATA_SC * bpsc;
        if n_data_bits > MAX_ENCODED_BITS {
            return None;
        }
        for j in 0..n_sym {
            // Per-symbol pilot wipe + polyfit
            let p = POLARITY[(j + 2) % 127]; // +2 for the 2 SIG symbols
            let psi = [
                PILOT_PSI[(j) % 4],
                PILOT_PSI[(1 + j) % 4],
                PILOT_PSI[(2 + j) % 4],
                PILOT_PSI[(3 + j) % 4],
            ];
            let h = &h_est;
            let mut pilots = [Complex32::new(0.0, 0.0); 4];
            for (k, &pi) in pilot_indices.iter().enumerate() {
                pilots[k] = syms[j][pi] / h[pi] * p * Complex32::new(psi[k], 0.0);
            }
            let mut sum_residual = Complex32::new(0.0, 0.0);
            for k in 0..4 {
                let prev_corr = accum_alpha * (pilot_xs[k] - x_mean) + accum_beta;
                sum_residual += pilots[k] * Complex32::from_polar(1.0, -prev_corr);
            }
            let residual_beta = sum_residual.arg();
            let residual_rot = Complex32::from_polar(1.0, -residual_beta);
            let mut sum_xc_phi = 0.0f32;
            let mut sum_xc2 = 0.0f32;
            for k in 0..4 {
                let prev_corr = accum_alpha * (pilot_xs[k] - x_mean) + accum_beta;
                let w_residual = pilots[k]
                    * Complex32::from_polar(1.0, -prev_corr)
                    * residual_rot;
                let xc = pilot_xs[k] - x_mean;
                sum_xc_phi += xc * w_residual.im;
                sum_xc2 += xc * xc;
            }
            let residual_alpha = if sum_xc2 > 0.0 {
                sum_xc_phi / sum_xc2
            } else {
                0.0
            };
            accum_alpha += residual_alpha;
            accum_beta += residual_beta;

            // Apply correction across all bins, then equalise + demap data subs
            for (idx, &off) in data_off.iter().enumerate() {
                let bin = sc(off);
                let pilot_eq_phase = -(accum_alpha * (off as f32 - x_mean) + accum_beta);
                let eq = syms[j][bin]
                    * Complex32::from_polar(1.0, pilot_eq_phase)
                    / h_est[bin];
                let bits = mcs.modulation().demap(&eq);
                let byte_idx = j * N_DATA_SC + idx;
                // Spread `bits` (bpsc bits) into rx_bits at byte_idx*bpsc
                for b in 0..bpsc {
                    self.rx_bits[byte_idx * bpsc + b] = (bits >> b) & 1;
                }
            }
        }

        // ── 7. Deinterleave + Viterbi + descrambler (existing code path) ──
        // Deinterleave (mirrors Decoder::deinterleave)
        let n_cbps = mcs.n_cbps();
        let n_col: usize = 13;
        let s = std::cmp::max(bpsc / 2, 1);
        let mut first = vec![0usize; n_cbps];
        let mut second = vec![0usize; n_cbps];
        for j in 0..n_cbps {
            first[j] = s * (j / s) + ((j + (n_col * j / n_cbps)) % s);
        }
        for i in 0..n_cbps {
            second[i] = n_col * i - (n_cbps - 1) * (n_col * i / n_cbps);
        }
        for i in 0..n_sym {
            for k in 0..n_cbps {
                self.deint_bits[i * n_cbps + second[first[k]]] = self.rx_bits[i * n_cbps + k];
            }
        }

        // Viterbi decode (hard-decision)
        self.viterbi
            .decode(frame.clone(), &self.deint_bits, &mut self.decoded_bits);

        // Descramble (matches Decoder::descramble)
        let mut state: u8 = 0;
        for v in self.out_bytes.iter_mut() {
            *v = 0;
        }
        for i in 0..7 {
            if self.decoded_bits[i] > 0 {
                state |= 1 << (6 - i);
            }
        }
        for i in 7..frame.psdu_size() * 8 + 8 {
            let feedback = u8::from((state & 64) > 0) ^ u8::from((state & 8) > 0);
            let bit = feedback ^ (self.decoded_bits[i] & 1);
            self.out_bytes[i / 8] |= bit << (i % 8);
            state = ((state << 1) & 0x7e) | feedback;
        }

        // Aggregation not supported here — return non-aggregated PSDU as-is.
        if aggregation {
            return None;
        }
        let psdu_end = frame.psdu_size() + 1;
        // Try padding lengths to find the right CRC-32 boundary
        for padding in 0..4 {
            if psdu_end < padding + 5 {
                break;
            }
            let end = psdu_end - padding;
            let crc = crc32fast::hash(&self.out_bytes[1..end]);
            if crc == 558161692 {
                return Some(self.out_bytes[1..end - 4].to_vec());
            }
        }
        // No valid CRC: return the full PSDU anyway (for inspection).
        Some(self.out_bytes[1..psdu_end].to_vec())
    }
}

