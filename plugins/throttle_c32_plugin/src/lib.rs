use futuresdr::blocks::Throttle;
use futuresdr::prelude::Complex32;

// Complex32 sibling of `throttle_plugin`, which is Throttle<u8> and so cannot
// sit in a c32 stream. Config is the rate in samples per second, matching the
// u8 plugin.
plugin_api::export_plugin! {
    name: "ThrottleC32",
    description: "Throttle<Complex32> block plugin",
    config: f64,
    create: |cfg, _id| {
        Throttle::<Complex32>::new(cfg)
    }
}
