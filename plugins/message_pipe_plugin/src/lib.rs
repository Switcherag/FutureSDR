use futuresdr::blocks::MessagePipe;

plugin_api::export_plugin! {
    name: "MessagePipe",
    description: "MessagePipe block plugin",
    config: mpsc :: Sender < Pmt >,
    create: |cfg, _id| {
        MessagePipe::new(cfg)
    }
}
