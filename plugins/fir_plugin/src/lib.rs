use futuresdr::blocks::Fir;

plugin_api::export_plugin! {
    name: "Fir",
    description: "Fir block plugin",
    config: UNSUPPORTED,
    create: |cfg, _id| {
        Fir::<u8>::new(cfg)
    }
}
