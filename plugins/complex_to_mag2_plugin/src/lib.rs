// Magnitude squared: Complex32 → f32 (norm_sqr)
//
// Config: ()

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
pub struct ComplexToMag2<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = f32> = DefaultCpuWriter<f32>,
> {
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = f32>> ComplexToMag2<I, O> {
    pub fn new() -> Self {
        Self {
            input: I::default(),
            output: O::default(),
        }
    }
}

#[doc(hidden)]
impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = f32>> Kernel
    for ComplexToMag2<I, O>
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let m = {
            let i = self.input.slice();
            let o = self.output.slice();
            let m = std::cmp::min(i.len(), o.len());
            for idx in 0..m {
                o[idx] = i[idx].norm_sqr();
            }
            m
        };

        if m > 0 {
            self.input.consume(m);
            self.output.produce(m);
        }

        if self.input.finished() && self.input.slice().is_empty() {
            io.finished = true;
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "ComplexToMag2",
    description: "Complex32 magnitude squared (norm_sqr)",
    config: (),
    create: |_cfg, _id| {
        ComplexToMag2::<DefaultCpuReader<Complex32>, DefaultCpuWriter<f32>>::new()
    }
}
