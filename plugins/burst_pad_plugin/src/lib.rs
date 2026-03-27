use futuresdr::blocks::BurstPad;

plugin_api::export_plugin! {
    name: "BurstPad",
    description: "BurstPad block plugin",
    config: (usize, usize, u8),
    create: |cfg, _id| {
        BurstPad::<u8>::new(cfg.0, cfg.1, cfg.2)
    }
}
