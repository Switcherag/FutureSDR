use futuresdr::blocks::Combine;

plugin_api::export_plugin! {
    name: "Combine",
    description: "Combine block plugin",
    config: u8,
    create: |cfg, _id| {
        Combine::<u8>::new(cfg)
    }
}
