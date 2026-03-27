use futuresdr::blocks::Split;

plugin_api::export_plugin! {
    name: "Split",
    description: "Split block plugin",
    config: u8,
    create: |cfg, _id| {
        Split::<u8>::new(cfg)
    }
}
