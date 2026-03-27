// Moving average for Complex32 streams.
// Copied from examples/wlan/src/moving_average.rs
//
// Config: usize — window length

use futuresdr::prelude::*;
use num_complex::Complex32;

const MAX_ITER: usize = 4000;

#[derive(Block)]
pub struct MovingAverageComplex<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = Complex32> = DefaultCpuWriter<Complex32>,
> {
    #[input]
    input: I,
    #[output]
    output: O,
    len: usize,
    pad: usize,
}

impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>>
    MovingAverageComplex<I, O>
{
    pub fn new(len: usize) -> Self {
        assert!(len > 0);
        Self {
            input: I::default(),
            output: O::default(),
            len,
            pad: len - 1,
        }
    }
}

#[doc(hidden)]
impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>> Kernel
    for MovingAverageComplex<I, O>
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _m: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let input = self.input.slice();
        let input_len = input.len();
        let out = self.output.slice();
        let out_len = out.len();

        if self.pad > 0 {
            let m = std::cmp::min(self.pad, out.len());
            out[0..m].fill(Complex32::new(0.0, 0.0));
            self.pad -= m;
            self.output.produce(m);

            if m < out_len {
                io.call_again = true;
            }
        } else {
            let m = std::cmp::min(
                std::cmp::min(MAX_ITER, (input_len + 1).saturating_sub(self.len)),
                out.len(),
            );

            if m > 0 {
                let mut sum: Complex32 = input[0..(self.len - 1)].iter().sum();
                for i in 0..m {
                    sum += input[i + self.len - 1];
                    out[i] = sum;
                    sum -= input[i];
                }
                self.input.consume(m);
                self.output.produce(m);
            }

            if self.input.finished() && m == (input_len + 1).saturating_sub(self.len) {
                io.finished = true;
            };
        }
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "MovingAverageComplex",
    description: "Moving average for Complex32 streams",
    config: usize,
    create: |cfg, _id| {
        MovingAverageComplex::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new(cfg)
    }
}
