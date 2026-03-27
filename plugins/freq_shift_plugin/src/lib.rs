// Frequency shift block: multiplies input by a rotating phasor.
//
// Config: (f64, f64) — (frequency_offset_hz, sample_rate_hz)
//
// Input/Output: Complex32 stream

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
pub struct FreqShift<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = Complex32> = DefaultCpuWriter<Complex32>,
> {
    phase: Complex32,
    phase_inc: Complex32,
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>>
    FreqShift<I, O>
{
    pub fn new(freq_offset: f64, sample_rate: f64) -> Self {
        let phase_inc = Complex32::from_polar(
            1.0,
            (2.0 * std::f64::consts::PI * freq_offset / sample_rate) as f32,
        );
        Self {
            phase: Complex32::new(1.0, 0.0),
            phase_inc,
            input: I::default(),
            output: O::default(),
        }
    }
}

#[doc(hidden)]
impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>> Kernel
    for FreqShift<I, O>
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
                self.phase *= self.phase_inc;
                o[idx] = self.phase * i[idx];
            }
            m
        };

        // Renormalize phase to prevent drift
        if m > 0 {
            let norm = self.phase.norm();
            self.phase /= norm;
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
    name: "FreqShift",
    description: "Frequency shift (rotating phasor multiplication)",
    config: (f64, f64),
    create: |cfg, _id| {
        FreqShift::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new(cfg.0, cfg.1)
    }
}
