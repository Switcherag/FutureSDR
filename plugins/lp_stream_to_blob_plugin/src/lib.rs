//! Decode a length-prefixed u8 stream (`[u32 LE len][len bytes]`) into
//! `Pmt::Blob` messages. Pair with `blob_to_lp_stream_plugin`.

use futuresdr::prelude::*;

const MAX_FRAME_BYTES: u32 = 16 * 1024;

#[derive(Block)]
#[message_outputs(out)]
pub struct LpStreamToBlob<I = DefaultCpuReader<u8>>
where
    I: CpuBufferReader<Item = u8>,
{
    #[input]
    input: I,
    /// Accumulated bytes still being parsed (header + body).
    pending: Vec<u8>,
}

impl<I> LpStreamToBlob<I>
where
    I: CpuBufferReader<Item = u8>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            pending: Vec::new(),
        }
    }
}

impl<I> Default for LpStreamToBlob<I>
where
    I: CpuBufferReader<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I> Kernel for LpStreamToBlob<I>
where
    I: CpuBufferReader<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let inp = self.input.slice();
        let n = inp.len();
        if n > 0 {
            self.pending.extend_from_slice(inp);
            self.input.consume(n);
        }

        // Drain as many complete frames as we have.
        loop {
            if self.pending.len() < 4 {
                break;
            }
            let len =
                u32::from_le_bytes([self.pending[0], self.pending[1], self.pending[2], self.pending[3]]);
            if len > MAX_FRAME_BYTES {
                // Resync: drop one byte and try again. Bridge bytes can
                // never desync in normal operation, but be defensive.
                self.pending.drain(0..1);
                continue;
            }
            let total = 4 + len as usize;
            if self.pending.len() < total {
                break;
            }
            let frame: Vec<u8> = self.pending.drain(0..total).skip(4).collect();
            mio.post("out", Pmt::Blob(frame)).await?;
        }

        if self.input.finished() && n == 0 {
            io.finished = true;
        }
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "LpStreamToBlob",
    description: "Length-prefixed u8 stream -> Pmt::Blob (msg)",
    config: (),
    create: |_cfg, _id| {
        LpStreamToBlob::<DefaultCpuReader<u8>>::new()
    }
}
