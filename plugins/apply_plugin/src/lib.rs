use futuresdr::blocks::Apply;

plugin_api::export_plugin! {
    name: "Apply",
    description: "Apply block plugin",
    config: u8,
    create: |cfg, _id| {
        Apply::<u8>::new(cfg)
    }
}
