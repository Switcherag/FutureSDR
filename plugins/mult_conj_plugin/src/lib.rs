// Conjugate multiply: two Complex32 inputs → Complex32 output
// out = in0 * conj(in1)
//
// Config: ()

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
pub struct MultConj<
    I0: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    I1: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = Complex32> = DefaultCpuWriter<Complex32>,
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
        I1: CpuBufferReader<Item = Complex32>,
        O: CpuBufferWriter<Item = Complex32>,
    > MultConj<I0, I1, O>
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
        I1: CpuBufferReader<Item = Complex32>,
        O: CpuBufferWriter<Item = Complex32>,
    > Kernel for MultConj<I0, I1, O>
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
                o[idx] = a[idx] * b[idx].conj();
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
    name: "MultConj",
    description: "Conjugate multiply: in0 * conj(in1)",
    config: (),
    create: |_cfg, _id| {
        MultConj::<DefaultCpuReader<Complex32>, DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new()
    }
}
