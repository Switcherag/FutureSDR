use futuresdr::blocks::Source;

plugin_api::export_plugin! {
    name: "Source",
    description: "Source block plugin",
    config: u8,
    create: |cfg, _id| {
        Source::<u8>::new(cfg)
    }
}
