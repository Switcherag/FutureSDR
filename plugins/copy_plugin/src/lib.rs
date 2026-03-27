use futuresdr::blocks::Copy;

plugin_api::export_plugin! {
    name: "Copy",
    description: "Copy block plugin",
    config: (),
    create: |_cfg, _id| {
        Copy::<u8>::new()
    }
}
