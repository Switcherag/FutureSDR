use futuresdr::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Source that reads samples from a shared ring buffer (cross-flowgraph bridge).
///
/// Polls the buffer periodically. When data is available it is drained and
/// produced on the output port.
#[derive(Block)]
pub struct BridgeSource {
    #[output]
    output: DefaultCpuWriter<u8>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSource {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self {
            output: DefaultCpuWriter::default(),
            buf,
        }
    }
}

impl Kernel for BridgeSource {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let o = self.output.slice();
        if o.is_empty() {
            return Ok(());
        }

        let mut buf = self.buf.lock().unwrap();
        let available = buf.len().min(o.len());
        if available > 0 {
            for (dst, src) in o.iter_mut().zip(buf.drain(..available)) {
                *dst = src;
            }
            drop(buf);
            self.output.produce(available);
        } else {
            drop(buf);
            // No data yet — sleep briefly to avoid busy-looping
            io.block_on(async {
                futuresdr::async_io::Timer::after(std::time::Duration::from_millis(50)).await;
            });
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "BridgeSource",
    description: "Reads samples from a shared ring buffer (cross-flowgraph bridge)",
    config: Arc<Mutex<VecDeque<u8>>>,
    create: |buf, _id| {
        BridgeSource::new(buf)
    }
}
