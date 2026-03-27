use futuresdr::blocks::WebsocketSink;

plugin_api::export_plugin! {
    name: "WebsocketSink",
    description: "WebsocketSink block plugin",
    config: (u32, WebsocketSinkMode),
    create: |cfg, _id| {
        WebsocketSink::<u8>::new(cfg.0, cfg.1)
    }
}
