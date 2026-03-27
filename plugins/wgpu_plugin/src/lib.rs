use futuresdr::blocks::Wgpu;

plugin_api::export_plugin! {
    name: "Wgpu",
    description: "Wgpu block plugin",
    config: (wgpu :: Instance, u64, usize, usize),
    create: |cfg, _id| {
        Wgpu::new(cfg.0, cfg.1, cfg.2, cfg.3)
    }
}
