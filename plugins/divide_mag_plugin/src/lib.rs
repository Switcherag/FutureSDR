// Divide magnitude: Complex32 + f32 inputs → f32 output
// out = in0.norm() / in1
//
// Config: ()

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
pub struct DivideMag<
    I0: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    I1: CpuBufferReader<Item = f32> = DefaultCpuReader<f32>,
    O: CpuBufferWriter<Item = f32> = DefaultCpuWriter<f32>,
> {
    #[input]
    in0: I0,
    #[input]
    in1: I1,
    #[output]
    output: O,
}

impl<
        I0: CpuBufferReader<Item = Complex32>,
        I1: CpuBufferReader<Item = f32>,
        O: CpuBufferWriter<Item = f32>,
    > DivideMag<I0, I1, O>
{
    pub fn new() -> Self {
        Self {
            in0: I0::default(),
            in1: I1::default(),
            output: O::default(),
        }
    }
}

#[doc(hidden)]
impl<
        I0: CpuBufferReader<Item = Complex32>,
        I1: CpuBufferReader<Item = f32>,
        O: CpuBufferWriter<Item = f32>,
    > Kernel for DivideMag<I0, I1, O>
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let m = {
            let a = self.in0.slice();
            let b = self.in1.slice();
            let o = self.output.slice();
            let m = a.len().min(b.len()).min(o.len());
            for idx in 0..m {
                o[idx] = a[idx].norm() / b[idx];
            }
            m
        };

        if m > 0 {
            self.in0.consume(m);
            self.in1.consume(m);
            self.output.produce(m);
        }

        if (self.in0.finished() && self.in0.slice().is_empty())
            || (self.in1.finished() && self.in1.slice().is_empty())
        {
            io.finished = true;
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "DivideMag",
    description: "Normalized magnitude: in0.norm() / in1",
    config: (),
    create: |_cfg, _id| {
        DivideMag::<DefaultCpuReader<Complex32>, DefaultCpuReader<f32>, DefaultCpuWriter<f32>>::new()
    }
}
