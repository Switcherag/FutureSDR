use futuresdr::blocks::MessageSink;

plugin_api::export_plugin! {
    name: "MessageSink",
    description: "MessageSink block plugin",
    config: (),
    create: |_cfg, _id| {
        MessageSink::new()
    }
}
