use futuresdr::blocks::Throttle;

plugin_api::export_plugin! {
    name: "Throttle",
    description: "Throttle block plugin",
    config: f64,
    create: |cfg, _id| {
        Throttle::<u8>::new(cfg)
    }
}
