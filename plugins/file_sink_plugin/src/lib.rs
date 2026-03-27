use futuresdr::blocks::FileSink;

plugin_api::export_plugin! {
    name: "FileSink",
    description: "FileSink block plugin",
    config: String,
    create: |cfg, _id| {
        FileSink::<u8>::new(cfg)
    }
}
