use futuresdr::blocks::BlobToUdp;

plugin_api::export_plugin! {
    name: "BlobToUdp",
    description: "BlobToUdp block plugin",
    config: String,
    create: |cfg, _id| {
        BlobToUdp::new(cfg)
    }
}
