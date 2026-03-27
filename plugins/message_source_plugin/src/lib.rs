use futuresdr::blocks::MessageSource;

plugin_api::export_plugin! {
    name: "MessageSource",
    description: "MessageSource block plugin",
    config: (Pmt, Duration, Option<usize>),
    create: |cfg, _id| {
        MessageSource::new(cfg.0, cfg.1, cfg.2)
    }
}
