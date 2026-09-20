//! A bladeRF 2.0 as the stream source, retuned by quick tune.
//!
//! The radio is driven through libbladeRF (the `bladerf` crate) rather than
//! seify/SoapySDR, which have no route to quick tune. At start, the radio is
//! tuned once to each channel the receivers may ask for, and the RFIC state
//! of each is kept (`bladerf_get_quick_tune`); a retune is then a recall of
//! that state (`bladerf_schedule_retune`), about 0.3 ms across bands on the
//! dyn branch's measurements, against 26.9 ms in FPGA tuning mode and
//! 111.6 ms in host mode.
//!
//! The source block reads the samples at the hardware rate, decimates them
//! to the rate the receivers expect, and takes the settings the receivers
//! ask for in their `[radio]` sections on message inputs `freq` and `gain`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use bladerf::BladeRF;
use bladerf::BladeRfAny;
use bladerf::Channel;
use bladerf::ChannelLayoutRx;
use bladerf::ComplexI16;
use bladerf::RxChannel;
use bladerf::RxSyncStream;
use bladerf::StreamConfig;
use bladerf::TuningMode;
use bladerf::sys::bladerf_channel;
use bladerf::sys::bladerf_get_quick_tune;
use bladerf::sys::bladerf_quick_tune;
use bladerf::sys::bladerf_schedule_retune;
use futuresdr::futuredsp::DecimatingFirFilter;
use futuresdr::futuredsp::firdes;
use futuresdr::futuredsp::prelude::*;
use futuresdr::runtime::dev::prelude::*;
use plugin_host::ReuseCpuWriter;

/// `BLADERF_RETUNE_NOW`, a C macro that bindgen does not emit.
const RETUNE_NOW: u64 = 0;

/// SC16 Q11: the AD9361's 12-bit samples, ±2048 for ±1.0.
const SCALE: f32 = 1.0 / 2048.0;

const CHANNEL: Channel = Channel::Rx0;

/// How the radio is set up.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Hardware sample rate, in samples per second.
    pub sample_rate: u32,
    /// Decimation to the receivers' rate: `sample_rate / decim`.
    pub decim: usize,
    pub gain_db: i32,
    /// Samples per USB transfer buffer (a multiple of 1024).
    pub buffer: usize,
    pub buffers: u32,
    pub transfers: u32,
    /// The channels to keep quick-tune profiles of, in Hz.
    pub channels: Vec<u64>,
    /// `set_frequency` instead of quick tune: what quick tune is worth.
    pub no_quick_tune: bool,
    /// Samples (at the receivers' rate) dropped after each retune, for those
    /// still in flight from the previous frequency.
    pub drop_after_retune: usize,
}

/// The opened radio and its quick-tune profiles.
pub struct Radio {
    dev: Arc<BladeRfAny>,
    profiles: Mutex<HashMap<u64, bladerf_quick_tune>>,
    pub settings: Settings,
    /// Retunes that found no profile and used `set_frequency`.
    pub misses: std::sync::atomic::AtomicU64,
}

// SAFETY: `bladerf_quick_tune` is a union of integers, and `BladeRfAny` is
// Send + Sync; the profiles are behind a mutex.
unsafe impl Send for Radio {}
unsafe impl Sync for Radio {}

impl Radio {
    /// Open the first bladeRF, set it up, and keep a quick-tune profile of
    /// each channel. Returns the radio and what the profiles are.
    pub fn open(settings: Settings) -> Result<(Arc<Self>, Vec<String>)> {
        let dev = Arc::new(BladeRfAny::open_first().context("opening the bladeRF")?);
        // The tuning mode first: changing it resets the RFIC, and with it the
        // rate, bandwidth and gain set before.
        dev.set_tuning_mode(TuningMode::FPGA)?;
        let rate = dev.set_sample_rate(CHANNEL, settings.sample_rate)?;
        if rate.abs_diff(settings.sample_rate) > 1 {
            bail!(
                "asked for {} S/s, the bladeRF gave {rate}",
                settings.sample_rate
            );
        }
        dev.set_bandwidth(CHANNEL, settings.sample_rate)?;
        dev.set_gain(CHANNEL, settings.gain_db)?;

        let ptr = dev.get_device_ptr();
        let mut profiles = HashMap::new();
        let mut report = Vec::new();
        for &hz in &settings.channels {
            // A profile is the RFIC's state on that channel, so it has to be
            // tuned there, and settled, first.
            dev.set_frequency(CHANNEL, hz)?;
            std::thread::sleep(Duration::from_millis(5));
            // SAFETY: an open device, and `qt` is the whole union.
            let mut qt: bladerf_quick_tune = unsafe { std::mem::zeroed() };
            let res = unsafe { bladerf_get_quick_tune(ptr, CHANNEL as bladerf_channel, &mut qt) };
            if res != 0 {
                bail!("bladerf_get_quick_tune at {hz} Hz failed: {res}");
            }
            // SAFETY: a bladeRF 2, whose arm of the union this is.
            let (nios, rffe) = unsafe {
                let p = qt.__bindgen_anon_1.__bindgen_anon_2;
                (p.nios_profile, p.rffe_profile)
            };
            report.push(format!(
                "{:9.3} MHz: NIOS profile {nios}, RFFE profile {rffe}",
                hz as f64 / 1e6
            ));
            profiles.insert(hz, qt);
        }
        let radio = Arc::new(Self {
            dev,
            profiles: Mutex::new(profiles),
            settings,
            misses: Default::default(),
        });
        Ok((radio, report))
    }

    pub fn serial(&self) -> String {
        self.dev.get_serial().unwrap_or_default()
    }

    /// Tune to `hz`: a profile recall if there is one within 1 kHz, else
    /// `set_frequency` (counted in `misses`). Returns how long it took.
    pub fn retune(&self, hz: f64) -> Result<Duration> {
        let t = Instant::now();
        let target = hz.round() as u64;
        let profile = if self.settings.no_quick_tune {
            None
        } else {
            let profiles = self.profiles.lock().unwrap();
            profiles
                .iter()
                .find(|(k, _)| k.abs_diff(target) < 1_000)
                .map(|(_, v)| *v)
        };
        match profile {
            Some(mut qt) => {
                // SAFETY: an open device, and a profile it gave.
                let res = unsafe {
                    bladerf_schedule_retune(
                        self.dev.get_device_ptr(),
                        CHANNEL as bladerf_channel,
                        RETUNE_NOW,
                        target,
                        &mut qt,
                    )
                };
                if res != 0 {
                    bail!("quick tune to {target} Hz failed: {res}");
                }
            }
            None => {
                if !self.settings.no_quick_tune {
                    self.misses
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                self.dev.set_frequency(CHANNEL, target)?;
            }
        }
        Ok(t.elapsed())
    }

    pub fn set_gain(&self, db: f64) -> Result<()> {
        Ok(self.dev.set_gain(CHANNEL, db.round() as i32)?)
    }

    fn stream(&self) -> Result<RxSyncStream<Arc<BladeRfAny>, ComplexI16, BladeRfAny>> {
        let s = &self.settings;
        let config = StreamConfig::new(
            s.buffers,
            s.buffer,
            s.transfers,
            Duration::from_millis(3500),
        )?;
        let stream = BladeRfAny::rx_streamer_arc::<ComplexI16>(
            self.dev.clone(),
            config,
            ChannelLayoutRx::SISO(RxChannel::Rx0),
        )?;
        stream.enable()?;
        Ok(stream)
    }
}

/// Retune times, for the report.
pub static RETUNES: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

/// Samples from the bladeRF, decimated; message inputs `freq` (Hz) and
/// `gain` (dB).
#[derive(Block)]
#[blocking]
#[message_inputs(freq, gain)]
pub struct BladeRfSource {
    #[output]
    output: ReuseCpuWriter<Complex32>,
    radio: Arc<Radio>,
    stream: Option<RxSyncStream<Arc<BladeRfAny>, ComplexI16, BladeRfAny>>,
    raw: Vec<ComplexI16>,
    fir: DecimatingFirFilter<Complex32, Complex32, Vec<f32>>,
    /// The last `taps - 1` input samples, and the new ones after them.
    history: Vec<Complex32>,
    n_history: usize,
    /// Decimated samples the output had no room for.
    pending: Vec<Complex32>,
    frequency: f64,
    /// Samples still to drop after a retune.
    drop: usize,
    /// Samples lost for want of room.
    pub overflows: u64,
}

impl BladeRfSource {
    pub fn new(radio: Arc<Radio>) -> Self {
        let decim = radio.settings.decim.max(1);
        // As FutureSDR's resampling FIR builder designs it.
        let taps: Vec<f32> = firdes::kaiser::multirate(1, decim, 12, 0.0001);
        let n_history = taps.len() - 1;
        Self {
            output: ReuseCpuWriter::default(),
            raw: vec![ComplexI16::new(0, 0); radio.settings.buffer],
            fir: DecimatingFirFilter::new(decim, taps),
            history: vec![Complex32::new(0.0, 0.0); n_history],
            n_history,
            pending: Vec::new(),
            frequency: f64::NAN,
            drop: 0,
            overflows: 0,
            stream: None,
            radio,
        }
    }

    async fn freq(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let hz = match p {
            Pmt::F64(v) => v,
            Pmt::F32(v) => v as f64,
            Pmt::U64(v) => v as f64,
            _ => return Ok(Pmt::InvalidValue),
        };
        if hz != self.frequency {
            let took = self.radio.retune(hz)?;
            RETUNES.lock().unwrap().push(took);
            self.frequency = hz;
            // What is held was received on the other channel.
            self.pending.clear();
            self.drop = self.radio.settings.drop_after_retune;
        }
        Ok(Pmt::Ok)
    }

    async fn gain(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let db = match p {
            Pmt::F64(v) => v,
            Pmt::F32(v) => v as f64,
            _ => return Ok(Pmt::InvalidValue),
        };
        self.radio.set_gain(db)?;
        Ok(Pmt::Ok)
    }
}

impl Kernel for BladeRfSource {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        self.stream = Some(self.radio.stream()?);
        Ok(())
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        // What the output had no room for last time, first.
        if !self.pending.is_empty() {
            let out = self.output.slice();
            let n = out.len().min(self.pending.len());
            out[..n].copy_from_slice(&self.pending[..n]);
            self.output.produce(n);
            self.pending.drain(..n);
        }

        // Always read: a radio that is not read overruns.
        let stream = self.stream.as_ref().expect("started");
        if stream
            .read(&mut self.raw, Duration::from_millis(200))
            .is_err()
        {
            io.call_again = true;
            return Ok(());
        }
        // `history` holds what the filter has not consumed yet: at least
        // the last `taps - 1` samples.
        let n_history = self.n_history;
        self.history.extend(
            self.raw
                .iter()
                .map(|s| Complex32::new(s.re as f32 * SCALE, s.im as f32 * SCALE)),
        );
        let decim = self.radio.settings.decim.max(1);
        let n_out = (self.history.len() - n_history) / decim;
        let mut decimated = vec![Complex32::new(0.0, 0.0); n_out];
        let (consumed, produced, _) = self.fir.filter(&self.history, &mut decimated);
        decimated.truncate(produced);
        self.history
            .drain(..consumed.min(self.history.len() - n_history));

        let mut samples = &decimated[..];
        if self.drop > 0 {
            let n = self.drop.min(samples.len());
            self.drop -= n;
            samples = &samples[n..];
        }
        let out = self.output.slice();
        let n = out.len().min(samples.len());
        out[..n].copy_from_slice(&samples[..n]);
        self.output.produce(n);
        // Keep a buffer's worth for when there is room; older ones are lost.
        let rest = &samples[n..];
        let keep = rest.len().min(self.raw.len());
        self.overflows += (rest.len() - keep) as u64;
        self.pending.extend_from_slice(&rest[rest.len() - keep..]);
        if self.pending.len() > self.raw.len() {
            let excess = self.pending.len() - self.raw.len();
            self.overflows += excess as u64;
            self.pending.drain(..excess);
        }
        io.call_again = true;
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        if let Some(stream) = self.stream.take() {
            stream.disable()?;
        }
        Ok(())
    }
}
