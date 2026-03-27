// DC offset correction using IIR filter.
//
// Config: f64 — ratio (e.g. 1e-5)

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
pub struct DcOffset<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = Complex32> = DefaultCpuWriter<Complex32>,
> {
    ratio: f64,
    avg_real: f64,
    avg_img: f64,
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>> DcOffset<I, O> {
    pub fn new(ratio: f64) -> Self {
        Self {
            ratio,
            avg_real: 0.0,
            avg_img: 0.0,
            input: I::default(),
            output: O::default(),
        }
    }
}

#[doc(hidden)]
impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>> Kernel
    for DcOffset<I, O>
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
                let c = i[idx];
                self.avg_real = self.ratio * (c.re as f64 - self.avg_real) + self.avg_real;
                self.avg_img = self.ratio * (c.im as f64 - self.avg_img) + self.avg_img;
                o[idx] = Complex32::new(
                    c.re - self.avg_real as f32,
                    c.im - self.avg_img as f32,
                );
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
    name: "DcOffset",
    description: "IIR DC offset correction for Complex32",
    config: f64,
    create: |cfg, _id| {
        DcOffset::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new(cfg)
    }
}
