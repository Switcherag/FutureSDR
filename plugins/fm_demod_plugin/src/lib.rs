// FM demodulator using the conjugate delay method.
// See https://en.wikipedia.org/wiki/Detector_(radio)#Quadrature_detector
//
// Config: () — no parameters needed
//
// Input:  Complex32 stream
// Output: f32 stream (instantaneous frequency)

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
pub struct FmDemod<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = f32> = DefaultCpuWriter<f32>,
> {
    last: Complex32,
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = f32>> FmDemod<I, O> {
    pub fn new() -> Self {
        Self {
            last: Complex32::new(0.0, 0.0),
            input: I::default(),
            output: O::default(),
        }
    }
}

#[doc(hidden)]
impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = f32>> Kernel
    for FmDemod<I, O>
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
                let v = i[idx];
                o[idx] = (v * self.last.conj()).arg();
                self.last = v;
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
    name: "FmDemod",
    description: "FM demodulator (conjugate delay method)",
    config: (),
    create: |_cfg, _id| {
        FmDemod::<DefaultCpuReader<Complex32>, DefaultCpuWriter<f32>>::new()
    }
}
