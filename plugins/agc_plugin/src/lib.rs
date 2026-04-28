// Automatic Gain Control (AGC) for Complex32 IQ streams.
//
// Adjusts the amplitude of incoming samples so the output settles at a
// configurable reference level.
//
// ## Algorithm
//
// This is a feed-forward, envelope-tracking AGC operating on instantaneous
// magnitude. For each sample:
//
// 1. **Measure** the instantaneous envelope: `|x[n]|` (magnitude of the
//    complex sample).
//
// 2. **Smooth** the envelope with an exponential moving average (EMA):
//
//        env[n] = (1 - α) · env[n-1] + α · |x[n]|
//
//    where α = `attack_rate` when the signal is rising (|x[n]| > env[n-1])
//    and α = `decay_rate` when the signal is falling. Separate rates let
//    the AGC react quickly to sudden power increases (attack) while
//    decaying slowly during brief dips (decay), avoiding "pumping" artifacts
//    where the gain oscillates audibly.
//
// 3. **Compute the gain**:
//
//        g[n] = reference / env[n]
//
//    clamped to [0, max_gain]. The clamp prevents runaway amplification
//    during silence or very weak signals.
//
// 4. **Apply** the gain:
//
//        y[n] = g[n] · x[n]
//
// The result is that the output amplitude converges toward `reference`,
// regardless of the input level (within the dynamic range set by max_gain).
//
// ## Why asymmetric attack/decay?
//
// A symmetric single-rate AGC has a fundamental trade-off:
// - Too fast → gain pumps on every symbol, distorting the signal
// - Too slow → can't track fading or bursty signals
//
// Asymmetric rates break this trade-off:
// - Fast attack (e.g. 1e-3): when a strong signal appears, gain drops
//   immediately to avoid clipping/saturation
// - Slow decay (e.g. 1e-5): when the signal dips briefly (between packets,
//   fading nulls), gain rises slowly, preventing noise blow-up
//
// ## Parameters
//
// | Parameter      | Default  | Role                                          |
// |---------------|----------|-----------------------------------------------|
// | reference     | 1.0      | Target output amplitude                       |
// | attack_rate   | 1e-3     | EMA rate when signal rises (fast response)    |
// | decay_rate    | 1e-5     | EMA rate when signal falls (slow release)     |
// | max_gain      | 65536.0  | Upper gain clamp (prevents noise blow-up)     |
// | initial_gain  | 1.0      | Starting gain before the loop converges       |
//
// ## Config
//
// Plugin config: `(f32, f32, f32, f32, f32)` =
//   (reference, attack_rate, decay_rate, max_gain, initial_gain)
//
// Input:  Complex32 stream
// Output: Complex32 stream (gain-adjusted)

use futuresdr::prelude::*;
use num_complex::Complex32;

#[derive(Block)]
#[message_inputs(reference, attack_rate, decay_rate, max_gain)]
pub struct Agc<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = Complex32> = DefaultCpuWriter<Complex32>,
> {
    #[input]
    input: I,
    #[output]
    output: O,
    ref_level: f32,
    atk_rate: f32,
    dec_rate: f32,
    gain_max: f32,
    gain: f32,
    envelope: f32,
}

impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>> Agc<I, O> {
    pub fn new(
        reference: f32,
        attack_rate: f32,
        decay_rate: f32,
        max_gain: f32,
        initial_gain: f32,
    ) -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            ref_level: reference,
            atk_rate: attack_rate,
            dec_rate: decay_rate,
            gain_max: max_gain,
            gain: initial_gain,
            envelope: 0.0,
        }
    }

    /// Create with default parameters (reference=1.0, attack=1e-3, decay=1e-5,
    /// max_gain=65536, initial_gain=1.0)
    pub fn default_params() -> Self {
        Self::new(1.0, 1e-3, 1e-5, 65536.0, 1.0)
    }

    async fn reference(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::F32(v) => {
                self.ref_level = v;
                Ok(Pmt::Ok)
            }
            Pmt::Null => Ok(Pmt::F32(self.ref_level)),
            _ => Ok(Pmt::InvalidValue),
        }
    }

    async fn attack_rate(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::F32(v) => {
                self.atk_rate = v;
                Ok(Pmt::Ok)
            }
            Pmt::Null => Ok(Pmt::F32(self.atk_rate)),
            _ => Ok(Pmt::InvalidValue),
        }
    }

    async fn decay_rate(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::F32(v) => {
                self.dec_rate = v;
                Ok(Pmt::Ok)
            }
            Pmt::Null => Ok(Pmt::F32(self.dec_rate)),
            _ => Ok(Pmt::InvalidValue),
        }
    }

    async fn max_gain(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::F32(v) => {
                self.gain_max = v;
                Ok(Pmt::Ok)
            }
            Pmt::Null => Ok(Pmt::F32(self.gain_max)),
            _ => Ok(Pmt::InvalidValue),
        }
    }
}

#[doc(hidden)]
impl<I: CpuBufferReader<Item = Complex32>, O: CpuBufferWriter<Item = Complex32>> Kernel
    for Agc<I, O>
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let i_len = i.len();

        let m = std::cmp::min(i_len, o.len());

        for k in 0..m {
            let mag = i[k].norm();

            // Asymmetric EMA: fast attack, slow decay
            let alpha = if mag > self.envelope {
                self.atk_rate
            } else {
                self.dec_rate
            };
            self.envelope = (1.0 - alpha) * self.envelope + alpha * mag;

            // Compute gain, clamp to gain_max
            self.gain = if self.envelope > 1e-20 {
                (self.ref_level / self.envelope).min(self.gain_max)
            } else {
                self.gain_max
            };

            o[k] = i[k] * self.gain;
        }

        self.input.consume(m);
        self.output.produce(m);

        if self.input.finished() && m == i_len {
            io.finished = true;
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "Agc",
    description: "Automatic Gain Control (envelope-tracking, asymmetric attack/decay)",
    config: (f32, f32, f32, f32, f32),
    create: |cfg, _id| {
        Agc::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new(
            cfg.0, cfg.1, cfg.2, cfg.3, cfg.4,
        )
    }
}
