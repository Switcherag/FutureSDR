// FIR Resampler plugin — wraps FirBuilder::resampling and resampling_with_taps.
//
// This plugin provides two sample types via the block_name:
// - "FirResamplerComplex": Complex32 → Complex32 resampling (for RF)
// - "FirResamplerReal":    f32 → f32 resampling with custom taps (for audio)
//
// Since a single export_plugin! can only export one block, we export the
// Complex32 resampler here. For the real-valued audio resampler, see
// fir_resampler_real_plugin.
//
// Config: (usize, usize) — (interpolation, decimation)

use futuresdr::blocks::FirBuilder;
use num_complex::Complex32;

plugin_api::export_plugin! {
    name: "FirResamplerComplex",
    description: "Polyphase FIR resampler for Complex32 streams",
    config: (usize, usize),
    create: |cfg, _id| {
        FirBuilder::resampling::<Complex32, Complex32>(cfg.0, cfg.1)
    }
}
