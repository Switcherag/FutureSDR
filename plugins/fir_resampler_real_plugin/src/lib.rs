// FIR Resampler (real-valued) with Kaiser window lowpass taps.
//
// Config: (usize, usize, f64, f64, f64)
//     (interpolation, decimation, cutoff, transition_width, attenuation)
//
// cutoff and transition_width are normalized to the output sample rate.
// attenuation is the stopband attenuation (e.g. 0.1 for -20dB).

use futuresdr::blocks::FirBuilder;
use futuresdr::futuredsp::firdes;

plugin_api::export_plugin! {
    name: "FirResamplerReal",
    description: "Polyphase FIR resampler for f32 streams with Kaiser lowpass",
    config: (usize, usize, f64, f64, f64),
    create: |cfg, _id| {
        let (interp, decim, cutoff, transition, attenuation) = cfg;
        let taps = firdes::kaiser::lowpass::<f32>(cutoff, transition, attenuation);
        FirBuilder::resampling_with_taps::<f32, f32, _>(interp, decim, taps)
    }
}
