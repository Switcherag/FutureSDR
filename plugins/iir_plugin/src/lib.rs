use futuresdr::blocks::Iir;

plugin_api::export_plugin! {
    name: "Iir",
    description: "Iir block plugin",
    config: (UNSUPPORTED, UNSUPPORTED),
    create: |cfg, _id| {
        Iir::<u8>::new(cfg.0, cfg.1)
    }
}
