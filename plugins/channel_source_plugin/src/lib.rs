use futuresdr::blocks::ChannelSource;

plugin_api::export_plugin! {
    name: "ChannelSource",
    description: "ChannelSource block plugin",
    config: mpsc :: Receiver < Box < [T] > >,
    create: |cfg, _id| {
        ChannelSource::<u8>::new(cfg)
    }
}
