use futuresdr::blocks::StreamDeinterleaver;

plugin_api::export_plugin! {
    name: "StreamDeinterleaver",
    description: "StreamDeinterleaver block plugin",
    config: usize,
    create: |cfg, _id| {
        StreamDeinterleaver::<u8>::new(cfg)
    }
}
