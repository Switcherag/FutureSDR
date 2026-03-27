use futuresdr::blocks::FileSource;

plugin_api::export_plugin! {
    name: "FileSource",
    description: "FileSource block plugin",
    config: (String, bool),
    create: |cfg, _id| {
        FileSource::<u8>::new(cfg.0, cfg.1)
    }
}
