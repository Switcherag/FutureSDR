use futuresdr::blocks::UdpSource;

plugin_api::export_plugin! {
    name: "UdpSource",
    description: "UdpSource block plugin",
    config: (String, usize),
    create: |cfg, _id| {
        UdpSource::<u8>::new(cfg.0, cfg.1)
    }
}
