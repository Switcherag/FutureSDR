use futuresdr::blocks::TagDebug;

plugin_api::export_plugin! {
    name: "TagDebug",
    description: "TagDebug block plugin",
    config: String,
    create: |cfg, _id| {
        TagDebug::<u8>::new(cfg)
    }
}
