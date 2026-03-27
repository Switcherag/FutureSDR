use futuresdr::blocks::MessageApply;

plugin_api::export_plugin! {
    name: "MessageApply",
    description: "MessageApply block plugin",
    config: u8,
    create: |cfg, _id| {
        MessageApply::<u8>::new(cfg)
    }
}
