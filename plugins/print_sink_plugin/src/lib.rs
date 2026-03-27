use futuresdr::prelude::*;

/// Sink that prints a label every time samples arrive.
#[derive(Block)]
pub struct PrintSink {
    #[input]
    input: DefaultCpuReader<u8>,
    label: String,
    total: usize,
}

impl PrintSink {
    pub fn new(label: String) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            label,
            total: 0,
        }
    }
}

impl Kernel for PrintSink {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let n = i.len();
        if n > 0 {
            self.total += n;
            println!("[{}] received {} samples (total: {})", self.label, n, self.total);
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "PrintSink",
    description: "Sink that prints a label on each sample batch",
    config: String,
    create: |label, _id| {
        PrintSink::new(label)
    }
}
