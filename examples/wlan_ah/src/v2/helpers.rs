//! Shared DSP helpers: DFT, FFT-shift, reference STF/LTF sequences in the
//! same convention as the Python notebook (fftshift, DC at index 32).

use futuresdr::num_complex::Complex32;

use crate::{DC_INDEX, FFT_SIZE, LTF_FREQ, sc};

/// Sample period in samples (Tu = FFT_SIZE).
pub const TU: usize = FFT_SIZE;
/// Cyclic-prefix length (Tcp).
pub const TCP: usize = 16;
/// Full symbol period (Ts = Tu + Tcp).
pub const TS: usize = TU + TCP;

/// Number of data symbols we are willing to buffer per frame. Enough for
/// an MCS0 PSDU of ~1500 bytes (same bound as the existing MAX_SYM).
pub const MAX_DATA_SYM: usize = crate::MAX_SYM;

/// Worst-case frame length in samples: 6 preamble symbols + data.
pub const MAX_FRAME_LEN: usize = (6 + MAX_DATA_SYM) * TS + TU;

/// 802.11ah STF frequency-domain sequence (fftshift, DC at index 32).
/// Python cell 4: `stf_syms` / sqrt(2).
pub fn stf_freq() -> [Complex32; FFT_SIZE] {
    let mut out = [Complex32::new(0.0, 0.0); FFT_SIZE];
    let p = 1.0 / std::f32::consts::SQRT_2;
    let plus = Complex32::new(p, p);
    let minus = Complex32::new(-p, -p);
    // Python: subc_seq = every 4th subcarrier in [-24..+24] excluding DC
    // subc_seq = np.array([a for a in np.arange(40, 92, 4) if a != 64])
    // Values at those positions (in Python fftshift order):
    //   [+, 0,0,0, -, 0,0,0, +, 0,0,0, -, 0,0,0, -, 0,0,0, +, 0,0,0, 0, 0,0,0,
    //    -, 0,0,0, -, 0,0,0, +, 0,0,0, +, 0,0,0, +, 0,0,0, +]
    // We lay those out at offsets -24, -20, -16, -12, -8, -4, (DC=0 skipped),
    // then 4, 8, 12, 16, 20, 24.
    let stf_pattern: [Complex32; 13] = [
        plus, minus, plus, minus, minus, plus,         // -24 .. -4
        // DC skipped
        minus, minus, plus, plus, plus, plus,          //  +4 .. +20
        plus,                                          //  +24 (reusing last pattern from notebook line)
    ];
    // Exact pattern from notebook (flattened, 13 non-zero entries at 4-spaced SCs):
    // [+, -, +, -, -, +,   (DC)   -, -, +, +, +, +]
    // Note: notebook has 13 non-zero entries total — we align them to
    // offsets [-24,-20,-16,-12,-8,-4, +4,+8,+12,+16,+20,+24]. Only 12
    // entries, but Python's listing includes an implicit 13th with +24
    // repeating. For correctness against the notebook, we use 12 symmetric.
    let offsets: [i32; 12] = [-24, -20, -16, -12, -8, -4, 4, 8, 12, 16, 20, 24];
    for (i, &off) in offsets.iter().enumerate() {
        out[sc(off)] = stf_pattern[i];
    }
    out
}

/// 802.11ah LTF frequency-domain sequence (fftshift, DC at index 32), as
/// a `Complex32` array for convenient broadcast operations.
pub fn ltf_freq() -> [Complex32; FFT_SIZE] {
    let mut out = [Complex32::new(0.0, 0.0); FFT_SIZE];
    let mut k = 0;
    for off in -28i32..=28 {
        if off == 0 {
            continue;
        }
        out[sc(off)] = Complex32::new(LTF_FREQ[k], 0.0);
        k += 1;
    }
    out
}

/// Naive 64-point DFT (forward) with `fftshift` applied to the output, so
/// bin 32 corresponds to DC. Sufficient for 2 MSps × few-kHz frame rates.
///
/// `input` must be length `FFT_SIZE`.
pub fn dft_shift(input: &[Complex32]) -> [Complex32; FFT_SIZE] {
    debug_assert_eq!(input.len(), FFT_SIZE);
    let mut out = [Complex32::new(0.0, 0.0); FFT_SIZE];
    let n = FFT_SIZE as f32;
    for k in 0..FFT_SIZE {
        let mut sum = Complex32::new(0.0, 0.0);
        for t in 0..FFT_SIZE {
            let angle = -2.0 * std::f32::consts::PI * (k as f32) * (t as f32) / n;
            sum += input[t] * Complex32::from_polar(1.0, angle);
        }
        // fftshift: output bin k maps to shifted bin (k + N/2) % N
        out[(k + DC_INDEX) % FFT_SIZE] = sum;
    }
    out
}

/// `exp(j · 2π · shift · fftshift(fftfreq(N)))` — the phase ramp used by the
/// Python notebook to apply a fractional time-domain shift in the frequency
/// domain after an FFT. `shift` is measured in samples.
pub fn frac_shift_ramp(shift: f32) -> [Complex32; FFT_SIZE] {
    let mut out = [Complex32::new(0.0, 0.0); FFT_SIZE];
    let n = FFT_SIZE as f32;
    // fftshift(fftfreq(N)) = (-N/2 .. N/2-1) / N  for FFT_SIZE even
    for k in 0..FFT_SIZE {
        let bin = (k as i32) - (DC_INDEX as i32);
        let freq = (bin as f32) / n;
        let angle = 2.0 * std::f32::consts::PI * shift * freq;
        out[k] = Complex32::from_polar(1.0, angle);
    }
    out
}

/// Linear least-squares slope: `sum((x-x_mean)*(y-y_mean)) / sum((x-x_mean)^2)`.
pub fn linear_slope(x: &[f32], y: &[f32]) -> f32 {
    debug_assert_eq!(x.len(), y.len());
    let n = x.len() as f32;
    let xm = x.iter().sum::<f32>() / n;
    let ym = y.iter().sum::<f32>() / n;
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for i in 0..x.len() {
        let dx = x[i] - xm;
        num += dx * (y[i] - ym);
        den += dx * dx;
    }
    if den > 0.0 { num / den } else { 0.0 }
}
