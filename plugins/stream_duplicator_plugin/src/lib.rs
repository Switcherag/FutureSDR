use futuresdr::blocks::StreamDuplicator;

plugin_api::export_plugin! {
    name: "StreamDuplicator",
    description: "StreamDuplicator block plugin",
    config: (),
    create: |_cfg, _id| {
        StreamDuplicator::<u8>::new()
    }
}
