//! Reference sequences for the v6 blocks.

use futuresdr::num_complex::Complex32;

use crate::{DC_INDEX, FFT_SIZE, LTF_FREQ, sc};

/// S1G LTF, frequency domain, fftshift order (DC at `DC_INDEX`).
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

/// Time-domain LTF, the matched-filter reference for `SyncLong`.
///
/// Derived from [`ltf_freq`] by inverse DFT rather than transcribed as a table,
/// so the correlator and the equaliser can never disagree about the sequence.
/// `dft_shift` maps natural bin `k` to shifted bin `(k + DC_INDEX) % N`, so the
/// inverse reads the shifted array back through that same mapping.
pub fn ltf_time() -> [Complex32; FFT_SIZE] {
    let f = ltf_freq();
    let n = FFT_SIZE as f32;
    let mut out = [Complex32::new(0.0, 0.0); FFT_SIZE];
    for (t, o) in out.iter_mut().enumerate() {
        let mut acc = Complex32::new(0.0, 0.0);
        for k in 0..FFT_SIZE {
            let x = f[(k + DC_INDEX) % FFT_SIZE];
            let angle = 2.0 * std::f32::consts::PI * (k as f32) * (t as f32) / n;
            acc += x * Complex32::from_polar(1.0, angle);
        }
        *o = acc / n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v5::helpers::dft_shift;

    /// `ltf_time` must be the exact inverse of the transform `FrameEqualizer`
    /// applies, or the correlator locks onto a sequence the equaliser does not
    /// recognise. Round-tripping catches a wrong fftshift convention, which is
    /// silent otherwise.
    #[test]
    fn ltf_time_round_trips_to_ltf_freq() {
        let td = ltf_time();
        let back = dft_shift(&td);
        let want = ltf_freq();
        for k in 0..FFT_SIZE {
            let err = (back[k] - want[k]).norm();
            assert!(
                err < 1e-3,
                "bin {k}: got {:?}, want {:?} (err {err})",
                back[k],
                want[k]
            );
        }
    }

    /// What `SyncLong` actually relies on: correlating the reference against
    /// two back-to-back LTF symbols peaks at offset 0 and offset FFT_SIZE, and
    /// the phase between those two peaks is what becomes the CFO estimate.
    #[test]
    fn ltf_autocorrelation_peaks_one_symbol_apart() {
        let td = ltf_time();
        let mut sig = Vec::with_capacity(3 * FFT_SIZE);
        sig.extend_from_slice(&td);
        sig.extend_from_slice(&td);
        sig.extend_from_slice(&td);

        let n = 2 * FFT_SIZE;
        let cor: Vec<f32> = (0..n)
            .map(|i| {
                let mut acc = Complex32::new(0.0, 0.0);
                for k in 0..FFT_SIZE {
                    acc += sig[i + k] * td[k].conj();
                }
                acc.norm()
            })
            .collect();

        let peak = cor[0];
        assert!(peak > 0.0, "zero-lag correlation vanished");
        assert!(
            (cor[FFT_SIZE] - peak).abs() < 1e-2 * peak,
            "repeat peak {} != zero-lag peak {peak}",
            cor[FFT_SIZE]
        );
        // Only 56 of 128 subcarriers are occupied, so the main lobe is
        // several samples wide and neighbouring lags are legitimately high.
        // What matters is that no *distant* lag rivals the peak, since
        // SyncLong takes the two largest and needs them to be the two symbol
        // starts rather than some unrelated position.
        const GUARD: usize = 8;
        for (lag, &v) in cor.iter().enumerate() {
            let near_peak = lag < GUARD
                || lag.abs_diff(FFT_SIZE) < GUARD
                || lag.abs_diff(2 * FFT_SIZE) < GUARD;
            if !near_peak {
                assert!(
                    v < 0.5 * peak,
                    "sidelobe at lag {lag} is {v}, too close to peak {peak}"
                );
            }
        }
    }
}
