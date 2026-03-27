use futuresdr::blocks::XlatingFir;

plugin_api::export_plugin! {
    name: "XlatingFir",
    description: "XlatingFir block plugin",
    config: (usize, f32, f32),
    create: |cfg, _id| {
        XlatingFir::<u8>::new(cfg.0, cfg.1, cfg.2)
    }
}
