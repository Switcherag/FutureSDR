// TODO: This block takes closure/function parameters.
//       Edit the `create` closure below to provide the concrete implementation.
use futuresdr::blocks::Filter;

plugin_api::export_plugin! {
    name: "Filter",
    description: "Filter block plugin",
    config: CLOSURE,
    create: |cfg, _id| {
        todo!("fill in closure params for Filter")
    }
}
