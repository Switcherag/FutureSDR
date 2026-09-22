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
//! A thread reads the samples at the hardware rate, and the source block
//! decimates them to the rate the receivers expect. [`Radio::tune`] retunes
//! from any thread while it reads (the example's swap loop, without waiting
//! for it); the block drops what was read before. The block also takes the
//! settings on message inputs `freq` and `gain`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use std::task::Poll;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context as _;
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
use futuresdr::futures::task::AtomicWaker;
use futuresdr::runtime::dev::prelude::*;
use plugin_host::ReuseCpuWriter;

/// `BLADERF_RETUNE_NOW`, a C macro that bindgen does not emit.
const RETUNE_NOW: u64 = 0;

/// SC16 Q11: the AD9361's 12-bit samples, ±2048 for ±1.0.
const SCALE: f32 = 1.0 / 2048.0;

const CHANNEL: Channel = Channel::Rx0;

/// Buffers the reader may fill ahead of the block: 13 ms at 20 MSps in
/// buffers of 4096, time for the block's thread to be late.
const READ_AHEAD: usize = 64;

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
    pub misses: AtomicU64,
    /// The frequency tuned to, in Hz (`f64` bits; NaN at first).
    frequency: AtomicU64,
    /// Retunes so far: samples read under an older count are of another
    /// channel.
    epoch: AtomicU64,
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
            frequency: AtomicU64::new(f64::NAN.to_bits()),
            epoch: AtomicU64::new(0),
        });
        Ok((radio, report))
    }

    pub fn serial(&self) -> String {
        self.dev.get_serial().unwrap_or_default()
    }

    /// Tune to `hz` unless it is tuned there already; returns how long it
    /// took if it retuned. Any thread may call it, while the source reads.
    pub fn tune(&self, hz: f64) -> Result<Option<Duration>> {
        if f64::from_bits(self.frequency.load(Ordering::Acquire)) == hz {
            return Ok(None);
        }
        let took = self.retune(hz)?;
        self.frequency.store(hz.to_bits(), Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        RETUNES.lock().unwrap().push(took);
        Ok(Some(took))
    }

    /// Retunes so far.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Tune to `hz`: a profile recall if there is one within 1 kHz, else
    /// `set_frequency` (counted in `misses`). Returns how long it took.
    fn retune(&self, hz: f64) -> Result<Duration> {
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
                    self.misses.fetch_add(1, Ordering::Relaxed);
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

/// Samples lost, in all (at the receivers' rate): read while all the
/// reader's buffers waited for the block.
pub static OVERFLOWS: AtomicU64 = AtomicU64::new(0);

/// Retune times, for the report.
pub static RETUNES: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

/// A buffer the reader filled, and the retunes there had been when it was.
struct Filled {
    samples: Vec<ComplexI16>,
    epoch: u64,
}

/// Ready when the reader has handed buffers over since last polled.
#[derive(Default)]
pub struct Handed {
    waker: AtomicWaker,
    ready: AtomicBool,
}

/// The block waits on this when it has done all the reader handed over.
pub struct Wait(Arc<Handed>);

impl Future for Wait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        self.0.waker.register(cx.waker());
        if self.0.ready.swap(false, Ordering::AcqRel) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// The thread reading the radio, and its way back.
struct Reader {
    thread: Option<JoinHandle<Result<()>>>,
    stop: Arc<AtomicBool>,
    filled: Receiver<Filled>,
    free: Sender<Vec<ComplexI16>>,
}

impl Reader {
    /// Stream from `radio` on a thread of its own, into [`READ_AHEAD`]
    /// buffers that the block hands back once done with them. When it has none to
    /// read into, the samples are lost, as a radio's are when not read.
    fn spawn(radio: Arc<Radio>, handed: Arc<Handed>) -> Result<Self> {
        let (filled_tx, filled) = mpsc::channel();
        let (free, free_rx) = mpsc::channel();
        let size = radio.settings.buffer;
        for _ in 0..READ_AHEAD {
            free.send(vec![ComplexI16::new(0, 0); size])?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("bladerf-rx".into())
            .spawn({
                let stop = stop.clone();
                move || -> Result<()> {
                    crate::replay::pin_source_thread()?;
                    let stream = radio.stream()?;
                    let decim = radio.settings.decim.max(1);
                    let mut spare = vec![ComplexI16::new(0, 0); size];
                    let mut held = None;
                    while !stop.load(Ordering::Relaxed) {
                        let mut buffer = held.take().or_else(|| free_rx.try_recv().ok());
                        let into = buffer.as_mut().unwrap_or(&mut spare);
                        if stream.read(into, Duration::from_millis(200)).is_err() {
                            held = buffer;
                            continue;
                        }
                        let Some(samples) = buffer else {
                            OVERFLOWS.fetch_add((size / decim) as u64, Ordering::Relaxed);
                            continue;
                        };
                        let epoch = radio.epoch();
                        if filled_tx.send(Filled { samples, epoch }).is_err() {
                            break;
                        }
                        handed.ready.store(true, Ordering::Release);
                        handed.waker.wake();
                    }
                    stream.disable()?;
                    Ok(())
                }
            })?;
        Ok(Self {
            thread: Some(thread),
            stop,
            filled,
            free,
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => bail!("the bladeRF reader panicked"),
            None => Ok(()),
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Samples from the bladeRF, decimated; message inputs `freq` (Hz) and
/// `gain` (dB).
///
/// A thread of its own reads the radio, so that the block waits for samples
/// rather than in the radio's driver, and takes its messages meanwhile.
#[derive(Block)]
#[message_inputs(freq, gain)]
#[blocking]
pub struct BladeRfSource {
    #[output]
    output: ReuseCpuWriter<Complex32>,
    radio: Arc<Radio>,
    reader: Option<Reader>,
    handed: Arc<Handed>,
    wait: Wait,
    fir: DecimatingFirFilter<Complex32, Complex32, Vec<f32>>,
    /// The last `taps - 1` input samples, and the new ones after them.
    history: Vec<Complex32>,
    n_history: usize,
    decimated: Vec<Complex32>,
    /// Decimated samples the output had no room for.
    pending: Vec<Complex32>,
    /// The retunes the samples of the last buffer were read after.
    epoch: u64,
    /// Samples still to drop after a retune.
    drop: usize,
}

impl BladeRfSource {
    pub fn new(radio: Arc<Radio>) -> Self {
        let decim = radio.settings.decim.max(1);
        // As FutureSDR's resampling FIR builder designs it.
        let taps: Vec<f32> = firdes::kaiser::multirate(1, decim, 12, 0.0001);
        let n_history = taps.len() - 1;
        let handed = Arc::new(Handed::default());
        Self {
            output: ReuseCpuWriter::default(),
            fir: DecimatingFirFilter::new(decim, taps),
            history: vec![Complex32::new(0.0, 0.0); n_history],
            n_history,
            decimated: Vec::new(),
            pending: Vec::new(),
            epoch: 0,
            drop: 0,
            reader: None,
            wait: Wait(handed.clone()),
            handed,
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
        self.radio.tune(hz)?;
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

    /// Decimate `raw` to the output; what does not fit waits in `pending`.
    fn push(&mut self, raw: &[ComplexI16]) {
        // `history` holds what the filter has not consumed yet: at least
        // the last `taps - 1` samples.
        let n_history = self.n_history;
        self.history.extend(
            raw.iter()
                .map(|s| Complex32::new(s.re as f32 * SCALE, s.im as f32 * SCALE)),
        );
        let decim = self.radio.settings.decim.max(1);
        let n_out = (self.history.len() - n_history) / decim;
        self.decimated.resize(n_out, Complex32::new(0.0, 0.0));
        let (consumed, produced, _) = self.fir.filter(&self.history, &mut self.decimated);
        self.history
            .drain(..consumed.min(self.history.len() - n_history));

        let mut samples = &self.decimated[..produced];
        if self.drop > 0 {
            let n = self.drop.min(samples.len());
            self.drop -= n;
            samples = &samples[n..];
        }
        let out = self.output.slice();
        let n = out.len().min(samples.len());
        out[..n].copy_from_slice(&samples[..n]);
        self.output.produce(n);
        self.pending.extend_from_slice(&samples[n..]);
    }
}

impl Kernel for BladeRfSource {
    type BlockOn = Wait;

    fn block_on(&mut self) -> Option<Pin<&mut Wait>> {
        Some(Pin::new(&mut self.wait))
    }

    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        // The radio's threads, away from the runtime's (see --cpus).
        crate::replay::pin_source_thread()?;
        self.epoch = self.radio.epoch();
        self.reader = Some(Reader::spawn(self.radio.clone(), self.handed.clone())?);
        Ok(())
    }

    async fn work(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        // What the output had no room for last time, first, unless of
        // another channel.
        let epoch = self.radio.epoch();
        if self.epoch != epoch {
            self.pending.clear();
        }
        if !self.pending.is_empty() {
            let out = self.output.slice();
            let n = out.len().min(self.pending.len());
            out[..n].copy_from_slice(&self.pending[..n]);
            self.output.produce(n);
            self.pending.drain(..n);
        }

        // Then what the reader handed over, while the output has room: the
        // rest waits with the reader, which loses samples only once all its
        // buffers are waiting, as a radio not read does. Called again when
        // the reader hands more (`block_on`) or the output has room.
        let reader = self.reader.take().expect("started");
        while self.pending.is_empty()
            && let Ok(Filled {
                samples,
                epoch: read,
            }) = reader.filled.try_recv()
        {
            // Read before the last retune: another channel's.
            if read == epoch {
                if self.epoch != epoch {
                    self.epoch = epoch;
                    self.pending.clear();
                    self.drop = self.radio.settings.drop_after_retune;
                }
                self.push(&samples);
            }
            let _ = reader.free.send(samples);
        }
        self.reader = Some(reader);
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        if let Some(mut reader) = self.reader.take() {
            reader.stop()?;
        }
        Ok(())
    }
}
