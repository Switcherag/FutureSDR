use futuresdr::blocks::VectorSource;

plugin_api::export_plugin! {
    name: "VectorSource",
    description: "VectorSource block plugin",
    config: Vec<u8>,
    create: |cfg, _id| {
        VectorSource::<u8>::new(cfg)
    }
}
