use futuresdr::blocks::ChannelSink;

plugin_api::export_plugin! {
    name: "ChannelSink",
    description: "ChannelSink block plugin",
    config: mpsc :: Sender < Box < [T] > >,
    create: |cfg, _id| {
        ChannelSink::<u8>::new(cfg)
    }
}
