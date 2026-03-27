use futuresdr::blocks::ApplyNM;

plugin_api::export_plugin! {
    name: "ApplyNM",
    description: "ApplyNM block plugin",
    config: u8,
    create: |cfg, _id| {
        ApplyNM::<u8>::new(cfg)
    }
}
