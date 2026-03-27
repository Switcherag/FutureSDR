use futuresdr::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_BRIDGE_ITEMS: usize = 4096;

/// Sink that pushes samples into a shared ring buffer for cross-flowgraph use.
///
/// When the buffer is full, oldest samples are dropped.
#[derive(Block)]
pub struct BridgeSink {
    #[input]
    input: DefaultCpuReader<u8>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSink {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            buf,
        }
    }
}

impl Kernel for BridgeSink {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let n = i.len();
        if n > 0 {
            let mut buf = self.buf.lock().unwrap();
            for &sample in i.iter() {
                if buf.len() >= MAX_BRIDGE_ITEMS {
                    buf.pop_front();
                }
                buf.push_back(sample);
            }
            drop(buf);
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "BridgeSink",
    description: "Pushes samples into a shared ring buffer (cross-flowgraph bridge)",
    config: Arc<Mutex<VecDeque<u8>>>,
    create: |buf, _id| {
        BridgeSink::new(buf)
    }
}
