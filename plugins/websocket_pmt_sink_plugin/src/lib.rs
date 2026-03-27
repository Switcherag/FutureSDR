use futuresdr::blocks::WebsocketPmtSink;

plugin_api::export_plugin! {
    name: "WebsocketPmtSink",
    description: "WebsocketPmtSink block plugin",
    config: u32,
    create: |cfg, _id| {
        WebsocketPmtSink::new(cfg)
    }
}
