//! Encode `Pmt::Blob` messages into a length-prefixed u8 stream so that
//! frame messages can travel across an inter-flowgraph stream bridge.
//!
//! Frame format on the wire: `[u32 LE len][len bytes payload]`. The matching
//! decoder is `lp_stream_to_blob_plugin`.

use futuresdr::prelude::*;
use std::collections::VecDeque;

#[derive(Block)]
#[message_inputs(rx)]
pub struct BlobToLpStream<O = DefaultCpuWriter<u8>>
where
    O: CpuBufferWriter<Item = u8>,
{
    #[output]
    output: O,
    pending: VecDeque<u8>,
}

impl<O> BlobToLpStream<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    pub fn new() -> Self {
        Self {
            output: O::default(),
            pending: VecDeque::new(),
        }
    }

    async fn rx(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Blob(b) => {
                if b.len() <= u32::MAX as usize {
                    let len = b.len() as u32;
                    self.pending.extend(len.to_le_bytes());
                    self.pending.extend(b.into_iter());
                    io.call_again = true;
                }
            }
            Pmt::Finished => {
                io.finished = true;
            }
            _ => {}
        }
        Ok(Pmt::Ok)
    }
}

impl<O> Default for BlobToLpStream<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<O> Kernel for BlobToLpStream<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    async fn work(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let out = self.output.slice();
        if out.is_empty() || self.pending.is_empty() {
            return Ok(());
        }
        let n = std::cmp::min(out.len(), self.pending.len());
        for slot in out.iter_mut().take(n) {
            *slot = self.pending.pop_front().unwrap();
        }
        self.output.produce(n);
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "BlobToLpStream",
    description: "Pmt::Blob (msg) -> length-prefixed u8 stream",
    config: (),
    create: |_cfg, _id| {
        BlobToLpStream::<DefaultCpuWriter<u8>>::new()
    }
}
