use futuresdr::blocks::FiniteSource;

plugin_api::export_plugin! {
    name: "FiniteSource",
    description: "FiniteSource block plugin",
    config: u8,
    create: |cfg, _id| {
        FiniteSource::<u8>::new(cfg)
    }
}
