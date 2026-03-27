use futuresdr::blocks::VectorSink;

plugin_api::export_plugin! {
    name: "VectorSink",
    description: "VectorSink block plugin",
    config: usize,
    create: |cfg, _id| {
        VectorSink::<u8>::new(cfg)
    }
}
