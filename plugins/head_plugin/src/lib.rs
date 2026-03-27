use futuresdr::blocks::Head;

plugin_api::export_plugin! {
    name: "Head",
    description: "Head block plugin",
    config: u64,
    create: |cfg, _id| {
        Head::<u8>::new(cfg)
    }
}
