use futuresdr::blocks::ZynqSync;

plugin_api::export_plugin! {
    name: "ZynqSync",
    description: "ZynqSync block plugin",
    config: (String, String, Vec<String>),
    create: |cfg, _id| {
        ZynqSync::<u8>::new(cfg.0, cfg.1, cfg.2)
    }
}
