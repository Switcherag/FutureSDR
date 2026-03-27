use futuresdr::blocks::ConsoleSink;

plugin_api::export_plugin! {
    name: "ConsoleSink",
    description: "ConsoleSink block plugin",
    config: String,
    create: |cfg, _id| {
        ConsoleSink::<u8>::new(cfg)
    }
}
