use futuresdr::blocks::Sink;

plugin_api::export_plugin! {
    name: "Sink",
    description: "Sink block plugin",
    config: u8,
    create: |cfg, _id| {
        Sink::<u8>::new(cfg)
    }
}
