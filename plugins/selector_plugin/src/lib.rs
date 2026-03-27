use futuresdr::blocks::Selector;

plugin_api::export_plugin! {
    name: "Selector",
    description: "Selector block plugin",
    config: DropPolicy,
    create: |cfg, _id| {
        Selector::<u8>::new(cfg)
    }
}
