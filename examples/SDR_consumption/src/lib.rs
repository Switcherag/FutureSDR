//! Shared helpers for the SDR power-consumption sweeps.
//!
//! Both binaries (`rx`, `tx`) walk the same parameter grid, holding each
//! configuration for a fixed dwell time so an external power meter can be
//! aligned against the emitted CSV schedule.
//!
//! Parameter grid:
//!   * frequency: 100 MHz, 1000 MHz
//!   * bandwidth: 1, 4, 16, 32 MHz  (mapped to the SDR *sample rate*)
//!   * gain:      0, 4, 16, 64 dB
//!   * signal (TX only): constant, sine, noise — all at full-scale amplitude
//!
//! The sine is a 0.2 ms-period (5 kHz) baseband tone. Its phase increment
//! depends on the sample rate, so it is recomputed whenever the bandwidth
//! changes (see [`TxSignalSource::rate`]).

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use rand::Rng;

/// Period of the sine test tone (0.2 ms).
pub const SINE_PERIOD_S: f32 = 0.2e-3;
/// Frequency of the sine test tone (5 kHz = 1 / 0.2 ms).
pub const SINE_FREQ_HZ: f32 = 1.0 / SINE_PERIOD_S;

/// Center frequencies to sweep (Hz).
pub const FREQS_HZ: [f64; 2] = [100e6, 1000e6];
/// Bandwidths to sweep (Hz) — applied as the SDR sample rate.
pub const BWS_HZ: [f64; 4] = [1e6, 4e6, 16e6, 32e6];
/// Gains to sweep (dB). Values beyond the device range are clamped by the driver.
pub const GAINS_DB: [f64; 4] = [0.0, 4.0, 16.0, 64.0];
/// Default dwell time per configuration (seconds).
pub const DWELL_SECS: f64 = 10.0;
/// Full-scale amplitude for the CF32 sample stream.
pub const FULL_SCALE: f32 = 1.0;

/// TX test-signal kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// DC at full scale (`1.0 + 0j`).
    Constant = 0,
    /// 5 kHz complex tone at full scale.
    Sine = 1,
    /// Complex uniform noise filling the full-scale square `[-1,1] + [-1,1]j`.
    Noise = 2,
}

impl Signal {
    /// All signal kinds, in sweep order.
    pub const ALL: [Signal; 3] = [Signal::Constant, Signal::Sine, Signal::Noise];

    /// Lower-case label for CSV / logs.
    pub fn label(self) -> &'static str {
        match self {
            Signal::Constant => "constant",
            Signal::Sine => "sine",
            Signal::Noise => "noise",
        }
    }

    /// Reconstruct from the `u32` index sent over the `mode` message port.
    pub fn from_index(i: u32) -> Signal {
        match i {
            0 => Signal::Constant,
            1 => Signal::Sine,
            _ => Signal::Noise,
        }
    }
}

/// Continuous full-scale TX source whose waveform and sample rate can be
/// switched live, so the flowgraph stays open across the whole sweep.
///
/// # Message inputs
///   * `mode`: `U32`/`U64`/`Usize` index into [`Signal`] — switch waveform.
///   * `rate`: `F32`/`F64` (Hz) — update the sample rate so the 5 kHz tone
///     keeps its 0.2 ms period at the new bandwidth.
#[derive(Block)]
#[message_inputs(mode, rate)]
pub struct TxSignalSource<O = DefaultCpuWriter<Complex32>>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    #[output]
    output: O,
    signal: Signal,
    amplitude: f32,
    /// Running sine phase (rad), kept in `[0, 2π)`.
    phase: f32,
    /// Per-sample phase increment `2π · 5kHz / sample_rate`.
    phase_inc: f32,
}

impl<O> TxSignalSource<O>
where
    O: CpuBufferWriter<Item = Complex32>,
{
    /// Create the source for an initial waveform / sample rate.
    pub fn new(signal: Signal, sample_rate: f64, amplitude: f32) -> Self {
        Self {
            output: O::default(),
            signal,
            amplitude,
            phase: 0.0,
            phase_inc: Self::phase_inc_for(sample_rate as f32),
        }
    }

    fn phase_inc_for(sample_rate: f32) -> f32 {
        core::f32::consts::TAU * SINE_FREQ_HZ / sample_rate
    }

    async fn mode(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let idx = match p {
            Pmt::U32(v) => v,
            Pmt::U64(v) => v as u32,
            Pmt::Usize(v) => v as u32,
            _ => return Ok(Pmt::InvalidValue),
        };
        self.signal = Signal::from_index(idx);
        Ok(Pmt::Ok)
    }

    async fn rate(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let sr = match p {
            Pmt::F64(v) => v as f32,
            Pmt::F32(v) => v,
            _ => return Ok(Pmt::InvalidValue),
        };
        self.phase_inc = Self::phase_inc_for(sr);
        Ok(Pmt::Ok)
    }
}

#[doc(hidden)]
impl<O> Kernel for TxSignalSource<O>
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
        let a = self.amplitude;

        match self.signal {
            Signal::Constant => {
                let v = Complex32::new(a, 0.0);
                for s in o.iter_mut() {
                    *s = v;
                }
            }
            Signal::Sine => {
                for s in o.iter_mut() {
                    *s = Complex32::new(a * self.phase.cos(), a * self.phase.sin());
                    self.phase += self.phase_inc;
                    if self.phase >= core::f32::consts::TAU {
                        self.phase -= core::f32::consts::TAU;
                    }
                }
            }
            Signal::Noise => {
                let mut r = rand::rng();
                for s in o.iter_mut() {
                    *s = Complex32::new(
                        a * r.random_range(-1.0f32..=1.0),
                        a * r.random_range(-1.0f32..=1.0),
                    );
                }
            }
        }

        self.output.produce(n);
        Ok(())
    }
}
