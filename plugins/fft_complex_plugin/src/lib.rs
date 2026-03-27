// FFT plugin for Complex32 streams.
//
// Config: (usize, bool, bool, Option<f32>)
//   — (fft_size, is_inverse, fft_shift, normalization)

use futuresdr::blocks::{Fft, FftDirection};
use futuresdr::prelude::{DefaultCpuReader, DefaultCpuWriter};
use futuresdr::num_complex::Complex32;

plugin_api::export_plugin! {
    name: "FftComplex",
    description: "FFT/IFFT for Complex32 streams",
    config: (usize, bool, bool, Option<f32>),
    create: |cfg, _id| {
        let (fft_size, is_inverse, fft_shift, norm) = cfg;
        let direction = if is_inverse { FftDirection::Inverse } else { FftDirection::Forward };
        Fft::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::with_options(fft_size, direction, fft_shift, norm)
    }
}
