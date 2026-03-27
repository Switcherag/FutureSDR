use futuresdr::blocks::TcpSink;

plugin_api::export_plugin! {
    name: "TcpSink",
    description: "TcpSink block plugin",
    config: u32,
    create: |cfg, _id| {
        TcpSink::<u8>::new(cfg)
    }
}
