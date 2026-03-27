use futuresdr::blocks::NullSource;

plugin_api::export_plugin! {
    name: "NullSource",
    description: "NullSource block plugin",
    config: (),
    create: |_cfg, _id| {
        NullSource::<u8>::new()
    }
}
