use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

/// Baseband oscillator whose instantaneous frequency is itself a sine wave.
///
/// `f_inst(t) = center_hz + deviation_hz * sin(2π · t / period_s)`
///
/// The output is `amplitude * exp(j · φ(t))` where φ is the running
/// integral of `2π · f_inst`. Both `t` and `φ` are advanced per-sample
/// (and wrapped) to avoid precision loss over long captures.
#[derive(Block)]
pub struct SineFmOscillator<O = DefaultCpuWriter<Complex32>>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    #[output]
    output: O,
    amplitude: f32,
    center_hz: f32,
    deviation_hz: f32,
    sweep_omega: f32,
    dt: f32,
    phase: f32,
    t: f32,
}

impl<O> SineFmOscillator<O>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new(
        center_hz: f32,
        deviation_hz: f32,
        period_s: f32,
        sample_rate_hz: f32,
        amplitude: f32,
    ) -> Self {
        Self {
            output: O::default(),
            amplitude,
            center_hz,
            deviation_hz,
            sweep_omega: std::f32::consts::TAU / period_s,
            dt: 1.0 / sample_rate_hz,
            phase: 0.0,
            t: 0.0,
        }
    }
}

#[doc(hidden)]
impl<O> Kernel for SineFmOscillator<O>
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

        let tau = std::f32::consts::TAU;
        let sweep_period = tau / self.sweep_omega;

        for v in o.iter_mut() {
            let f_inst = self.center_hz + self.deviation_hz * (self.sweep_omega * self.t).sin();
            *v = Complex32::from_polar(self.amplitude, self.phase);

            self.phase += tau * f_inst * self.dt;
            if self.phase > tau {
                self.phase -= tau;
            } else if self.phase < -tau {
                self.phase += tau;
            }

            self.t += self.dt;
            if self.t >= sweep_period {
                self.t -= sweep_period;
            }
        }

        self.output.produce(n);
        Ok(())
    }
}
