use futuresdr::blocks::Delay;

plugin_api::export_plugin! {
    name: "Delay",
    description: "Delay block plugin",
    config: isize,
    create: |cfg, _id| {
        Delay::<u8>::new(cfg)
    }
}
