use futuresdr::blocks::Vulkan;

plugin_api::export_plugin! {
    name: "Vulkan",
    description: "Vulkan block plugin",
    config: (Instance, EntryPoint, u32),
    create: |cfg, _id| {
        Vulkan::<u8>::new(cfg.0, cfg.1, cfg.2)
    }
}
