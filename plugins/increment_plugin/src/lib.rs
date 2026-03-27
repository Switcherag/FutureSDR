use futuresdr::prelude::*;

#[derive(Block)]
pub struct IncrementU8<I = DefaultCpuReader<u8>, O = DefaultCpuWriter<u8>>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = u8>,
{
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<I, O> IncrementU8<I, O>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = u8>,
{
    pub fn new() -> Self {
        IncrementU8 {
            input: I::default(),
            output: O::default(),
        }
    }
}

impl<I, O> Kernel for IncrementU8<I, O>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = u8>,
{
    async fn work(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let input = self.input.slice();
        let out = self.output.slice();
        let n = std::cmp::min(input.len(), out.len());

        for i in 0..n {
            out[i] = input[i].wrapping_add(1);
        }

        self.input.consume(n);
        self.output.produce(n);

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "IncrementU8",
    description: "Increment each u8 value by 1",
    config: (),
    create: |_cfg, _id| {
        IncrementU8::<DefaultCpuReader<u8>, DefaultCpuWriter<u8>>::new()
    }
}
