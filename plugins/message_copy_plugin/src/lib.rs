use futuresdr::blocks::MessageCopy;

plugin_api::export_plugin! {
    name: "MessageCopy",
    description: "MessageCopy block plugin",
    config: (),
    create: |_cfg, _id| {
        MessageCopy::new()
    }
}
