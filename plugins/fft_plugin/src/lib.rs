use futuresdr::blocks::Fft;

plugin_api::export_plugin! {
    name: "Fft",
    description: "Fft block plugin",
    config: usize,
    create: |cfg, _id| {
        Fft::<u64>::new(cfg)
    }
}
