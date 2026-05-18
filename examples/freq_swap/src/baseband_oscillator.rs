use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

/// Complex baseband oscillator with a `freq` message input.
///
/// Produces `exp(j * 2π * f / fs * n) * amplitude`. The frequency can be
/// changed at runtime by sending a `Pmt::F32`/`Pmt::F64` (Hz) to the
/// `freq` message port — the phase is preserved across changes so the
/// transition is continuous (no glitch).
#[derive(Block)]
#[message_inputs(freq)]
pub struct BasebandOscillator<O = DefaultCpuWriter<Complex32>>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    #[output]
    output: O,
    sample_rate: f32,
    amplitude: f32,
    phase: f32,
    phase_inc: f32,
}

impl<O> BasebandOscillator<O>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new(frequency_hz: f32, sample_rate_hz: f32, amplitude: f32) -> Self {
        Self {
            output: O::default(),
            sample_rate: sample_rate_hz,
            amplitude,
            phase: 0.0,
            phase_inc: std::f32::consts::TAU * frequency_hz / sample_rate_hz,
        }
    }

    async fn freq(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let f = match &p {
            Pmt::F32(v) => *v,
            Pmt::F64(v) => *v as f32,
            Pmt::U32(v) => *v as f32,
            Pmt::U64(v) => *v as f32,
            other => {
                warn!("BasebandOscillator: unsupported freq Pmt: {other:?}");
                return Ok(Pmt::Null);
            }
        };
        self.phase_inc = std::f32::consts::TAU * f / self.sample_rate;
        Ok(Pmt::Ok)
    }
}

#[doc(hidden)]
impl<O> Kernel for BasebandOscillator<O>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let o = self.output.slice();
        let n = o.len();

        for v in o.iter_mut() {
            *v = Complex32::from_polar(self.amplitude, self.phase);
            self.phase += self.phase_inc;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            } else if self.phase < -std::f32::consts::TAU {
                self.phase += std::f32::consts::TAU;
            }
        }

        self.output.produce(n);
        Ok(())
    }
}
