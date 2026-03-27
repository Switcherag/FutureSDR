use futuresdr::blocks::{Selector, SelectorDropPolicy};

plugin_api::export_plugin! {
    name: "Selector_1_2",
    description: "Selector<u8, 1, 2> — 1 input, 2 outputs",
    config: SelectorDropPolicy,
    create: |cfg, _id| {
        Selector::<u8, 1, 2>::new(cfg)
    }
}
