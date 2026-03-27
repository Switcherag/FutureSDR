use futuresdr::blocks::MovingAvg;

plugin_api::export_plugin! {
    name: "MovingAvg",
    description: "MovingAvg block plugin",
    config: (f32, usize),
    create: |cfg, _id| {
        MovingAvg::<u8>::new(cfg.0, cfg.1)
    }
}
