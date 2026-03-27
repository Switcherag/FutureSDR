use futuresdr::blocks::TcpSource;

plugin_api::export_plugin! {
    name: "TcpSource",
    description: "TcpSource block plugin",
    config: String,
    create: |cfg, _id| {
        TcpSource::<u8>::new(cfg)
    }
}
