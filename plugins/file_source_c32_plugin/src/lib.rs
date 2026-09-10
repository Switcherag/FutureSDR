use futuresdr::blocks::FileSource;
use futuresdr::prelude::Complex32;

// Complex32 sibling of `file_source_plugin`, which is FileSource<u8> and so
// cannot sit at the head of a c32 stream. Config matches the u8 plugin:
// (path, repeat).
//
// This is what lets a recorded IQ capture stand in for the radio in a head
// flowgraph — see `examples/real_device_swap/flows/samples_head.toml`, which
// pairs it with `throttle_c32_plugin` to replay a file at its true rate.
plugin_api::export_plugin! {
    name: "FileSourceC32",
    description: "FileSource<Complex32> block plugin",
    config: (String, bool),
    create: |cfg, _id| {
        FileSource::<Complex32>::new(cfg.0, cfg.1)
    }
}
