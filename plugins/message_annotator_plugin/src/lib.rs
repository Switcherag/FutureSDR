use futuresdr::blocks::MessageAnnotator;

plugin_api::export_plugin! {
    name: "MessageAnnotator",
    description: "MessageAnnotator block plugin",
    config: (HashMap < String , Pmt >, Option<String>),
    create: |cfg, _id| {
        MessageAnnotator::new(cfg.0, cfg.1)
    }
}
