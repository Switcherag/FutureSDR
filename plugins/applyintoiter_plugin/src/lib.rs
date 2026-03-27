use futuresdr::blocks::ApplyIntoIter;

plugin_api::export_plugin! {
    name: "ApplyIntoIter",
    description: "ApplyIntoIter block plugin",
    config: u8,
    create: |cfg, _id| {
        ApplyIntoIter::<u8>::new(cfg)
    }
}
