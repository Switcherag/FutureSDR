// AudioSink plugin.
//
// Config: (u32, u16) — (sample_rate, channels)

use futuresdr::blocks::audio::AudioSink;
use futuresdr::prelude::DefaultCpuReader;

plugin_api::export_plugin! {
    name: "AudioSink",
    description: "Audio output sink (speaker/headphones)",
    config: (u32, u16),
    create: |cfg, _id| {
        AudioSink::<DefaultCpuReader<f32>>::new(cfg.0, cfg.1)
    }
}
