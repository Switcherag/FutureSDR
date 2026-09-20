//! Recorded frames in place of the radio.
//!
//! One 802.11ah (H) and one 802.15.4 (Z) recording, at 4 MSps, replayed
//! H Z H Z ... on the wall clock. The gap after an H frame is the time the
//! receiver has to change from HaLow to ZigBee (and retune from 919 MHz to
//! 2.425 GHz); the gap after a Z frame, the time to change back. Each step
//! of a run replays one stream, with the gap after H of that step.
//!
//! A `Tune` block after the replay offers the `frequency` control a radio
//! would: setting it takes as long as the radio's retune, and the samples
//! that arrive meanwhile are lost.

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use futuresdr::runtime::Timer;
use futuresdr::runtime::dev::prelude::*;
use plugin_api::BlockType;
use plugin_api::Plugin;
use plugin_api::add_kernel;
use plugin_host::ReuseCpuReader;
use plugin_host::ReuseCpuWriter;

pub const RATE: f64 = 4e6;
/// Silence before the first frame and after the last.
const LEAD: Duration = Duration::from_millis(50);

/// The streams to replay, by step; the `Replay` block takes its `step`.
static STREAMS: Mutex<Vec<Arc<Vec<Complex32>>>> = Mutex::new(Vec::new());
/// When the replay of the current step emitted its first sample.
pub static STARTED: Mutex<Option<Instant>> = Mutex::new(None);
/// Samples the replay emits at once, as a radio delivers them.
pub static CHUNK: AtomicUsize = AtomicUsize::new(256);
/// How long each `Tune` retune took (always its delay; for the report).
pub static RETUNES: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

/// Seconds since the replay of the current step began.
pub fn now() -> f64 {
    STARTED
        .lock()
        .unwrap()
        .map_or(0.0, |start| start.elapsed().as_secs_f64())
}

/// One transmitted frame: its PHY (0 H, 1 Z) and when it began and ended,
/// in seconds from the first replayed sample.
#[derive(Debug, Clone, Copy)]
pub struct Sent {
    pub phy: usize,
    pub start: f64,
    pub end: f64,
}

/// A replay run: one stream per step, and what each sent when.
pub struct Steps {
    pub ifs_h2z_ms: Vec<f64>,
    pub ifs_z2h_ms: f64,
    pub sent: Vec<Vec<Sent>>,
}

pub fn read_cf32(path: &Path) -> Result<Vec<Complex32>> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|[a, b, c, d, e, f, g, h]| {
            Complex32::new(
                f32::from_le_bytes([*a, *b, *c, *d]),
                f32::from_le_bytes([*e, *f, *g, *h]),
            )
        })
        .collect())
}

/// Prepare the streams: `frames` frames per step, H first, for each gap
/// after H in `ifs_h2z_ms`, and `ifs_z2h_ms` after Z.
pub fn prepare(
    halow: &[Complex32],
    zigbee: &[Complex32],
    frames: usize,
    ifs_h2z_ms: Vec<f64>,
    ifs_z2h_ms: f64,
) -> Steps {
    let samples = |ms: f64| (ms.max(0.0) / 1e3 * RATE).round() as usize;
    let n_lead = samples(LEAD.as_secs_f64() * 1e3);
    let mut streams = STREAMS.lock().unwrap();
    streams.clear();
    let mut sent_per_step = Vec::new();
    for &h2z in &ifs_h2z_ms {
        let gaps = [samples(h2z), samples(ifs_z2h_ms)];
        let mut out = vec![Complex32::default(); n_lead];
        let mut sent = Vec::new();
        for i in 0..frames {
            let phy = i % 2;
            let start = out.len();
            out.extend_from_slice(if phy == 0 { halow } else { zigbee });
            sent.push(Sent {
                phy,
                start: start as f64 / RATE,
                end: out.len() as f64 / RATE,
            });
            let gap = if i + 1 == frames { n_lead } else { gaps[phy] };
            out.resize(out.len() + gap, Complex32::default());
        }
        streams.push(Arc::new(out));
        sent_per_step.push(sent);
    }
    Steps {
        ifs_h2z_ms,
        ifs_z2h_ms,
        sent: sent_per_step,
    }
}

/// The radio flowgraph of step `step`: the replay, and a front end that
/// takes `retune_us` to change frequency.
pub fn head(step: usize, retune_us: u64) -> String {
    format!(
        r#"
        name = "radio"
        connections = "replay > tune"
        [blocks.replay]
        type = "Replay"
        step = {step}
        [blocks.tune]
        type = "Tune"
        delay_us = {retune_us}
        [outputs]
        samples = {{ port = "tune.output", type = "c32" }}
        [controls]
        frequency = "tune.freq"
        "#
    )
}

/// Emits a stream at `RATE` samples per second of wall clock, in whole
/// chunks, from its first call on.
#[derive(Block)]
struct Replay {
    #[output]
    output: ReuseCpuWriter<Complex32>,
    samples: Arc<Vec<Complex32>>,
    pos: usize,
    start: Option<Instant>,
    timer: Option<Timer>,
    /// The most room its output ever had.
    room: usize,
}

impl Kernel for Replay {
    type BlockOn = Timer;

    fn block_on(&mut self) -> Option<std::pin::Pin<&mut Timer>> {
        self.timer.as_mut().map(std::pin::Pin::new)
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let start = *self.start.get_or_insert_with(|| {
            let now = Instant::now();
            *STARTED.lock().unwrap() = Some(now);
            now
        });
        let chunk = CHUNK.load(Ordering::Relaxed);
        let len = self.samples.len();
        let due = ((start.elapsed().as_secs_f64() * RATE) as usize).min(len);
        let owed = due - self.pos.min(due);
        let ready = if due == len {
            owed
        } else {
            owed - owed % chunk
        };
        let out = self.output.slice();
        self.room = self.room.max(out.len());
        let mut n = ready.min(out.len());
        if n < ready && n < chunk.min(self.room) {
            n = 0;
        }
        out[..n].copy_from_slice(&self.samples[self.pos..self.pos + n]);
        self.output.produce(n);
        self.pos += n;
        self.timer = None;
        if self.pos == len {
            io.finished = true;
        } else if n == ready {
            let next = self.pos + chunk - (self.pos + chunk) % chunk;
            let at = start + Duration::from_secs_f64(next.min(len) as f64 / RATE);
            self.timer = Some(Timer::after(at.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }
}

/// Passes samples on; message input `freq` takes `delay` to set, and the
/// samples that arrive meanwhile are lost, as with a radio front end.
#[derive(Block)]
#[message_inputs(freq)]
struct Tune {
    #[input]
    input: ReuseCpuReader<Complex32>,
    #[output]
    output: ReuseCpuWriter<Complex32>,
    delay: Duration,
    freq: f64,
    drop: bool,
}

impl Tune {
    async fn freq(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let Pmt::F64(f) = p else {
            return Ok(Pmt::InvalidValue);
        };
        if f != self.freq {
            let t = Instant::now();
            Timer::after(self.delay).await;
            RETUNES.lock().unwrap().push(t.elapsed());
            self.freq = f;
            self.drop = true;
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for Tune {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let len = i.len();
        if std::mem::take(&mut self.drop) {
            self.input.consume(len);
        } else {
            let o = self.output.slice();
            let n = len.min(o.len());
            o[..n].copy_from_slice(&i[..n]);
            self.input.consume(n);
            self.output.produce(n);
            if n < len {
                io.call_again = true;
            }
        }
        if self.input.finished() && self.input.slice().is_empty() {
            io.finished = true;
        }
        Ok(())
    }
}

/// The `Replay` and `Tune` block types.
pub fn blocks() -> Plugin {
    Plugin::new(
        "replay",
        vec![
            BlockType {
                name: "Replay".into(),
                description: "A prepared stream (setting `step`), at 4 MSps.",
                add: |fg, s| {
                    let step: usize = s.get("step")?;
                    let samples = STREAMS.lock().unwrap()[step].clone();
                    add_kernel(
                        fg,
                        Replay {
                            output: ReuseCpuWriter::default(),
                            samples,
                            pos: 0,
                            start: None,
                            timer: None,
                            room: 0,
                        },
                    )
                },
            },
            BlockType {
                name: "Tune".into(),
                description: "A front end that takes `delay_us` to retune.",
                add: |fg, s| {
                    add_kernel(
                        fg,
                        Tune {
                            input: ReuseCpuReader::default(),
                            output: ReuseCpuWriter::default(),
                            delay: Duration::from_micros(s.get("delay_us")?),
                            freq: f64::NAN,
                            drop: false,
                        },
                    )
                },
            },
        ],
    )
}
