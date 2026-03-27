use futuresdr::blocks::MessageBurst;

plugin_api::export_plugin! {
    name: "MessageBurst",
    description: "MessageBurst block plugin",
    config: (Pmt, u64),
    create: |cfg, _id| {
        MessageBurst::new(cfg.0, cfg.1)
    }
}
