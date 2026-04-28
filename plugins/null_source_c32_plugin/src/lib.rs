use futuresdr::blocks::NullSource;
use futuresdr::prelude::Complex32;

plugin_api::export_plugin! {
    name: "NullSourceC32",
    description: "NullSource<Complex32> block plugin",
    config: (),
    create: |_cfg, _id| {
        NullSource::<Complex32>::new()
    }
}
