use futuresdr::blocks::Zynq;

plugin_api::export_plugin! {
    name: "Zynq",
    description: "Zynq block plugin",
    config: (String, String, Vec<String>),
    create: |cfg, _id| {
        Zynq::<u8>::new(cfg.0, cfg.1, cfg.2)
    }
}
