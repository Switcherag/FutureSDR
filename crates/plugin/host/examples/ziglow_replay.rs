//! Packet error rate of a receiver that changes PHY after every frame,
//! against the time between frames: the `dyn` branch's `ziglow_replay`.
//!
//! ```text
//! replay (recorded frames, H Z H Z ..., paced at 4 MSps) ──samples──▶ HaLow / ZigBee
//! ```
//!
//! For each inter-frame spacing (IFS) of the sweep, a stream of recorded
//! 802.11ah (H) and 802.15.4 (Z) frames, alternating and separated by IFS, is
//! replayed at its sample rate. Each time a receiver posts a frame, the
//! controller switches to the other PHY, so the switch must be done before
//! the next frame starts. Every copy of a frame is the same recording, so
//! PER is `1 - received / sent` per PHY.
//!
//! A receiver that is switched away from keeps what it already holds, and a
//! replaced one drains in the background, so both can still post frames
//! after the switch. Those are frames of what was sent *before* it, which a
//! radio would have received too. To make sure none of them is a frame the
//! transmitter sent after moving to the other PHY, each frame is matched to
//! the transmission it decodes (by the time it arrives, decoding takes about
//! 40 µs) and counted only if its receiver was listening while that
//! transmission was on the air. The rest are reported as `impossible` and
//! left out.
//!
//! Switching modes (`--mode`, `all` runs each):
//!
//! - `select`: both receivers run; their links from the replay are selected
//!   in turn (`Controller::select`), the other one parked.
//! - `standby`: one receiver flowgraph; the other PHY is prepared ahead and
//!   committed (`Controller::commit`).
//! - `replace`: one receiver flowgraph, replaced on demand
//!   (`Controller::replace`), as `dyn` does.
//! - `both`: both receivers get every sample; no switching (the reference).
//!
//! With `--retune-us N`, the replay offers a `frequency` control that takes
//! N µs to set and loses the samples meanwhile, as a radio front end would;
//! the receivers ask for their channel in their `[radio]` sections.
//!
//! ```text
//! cd crates/plugin
//! cargo run --release --example ziglow_replay -- --mode all
//! ```
//!
//! Options: `--mode select|standby|replace|both|all`, `--frames N` (per IFS,
//! default 200), `--ifs-start MS` (4), `--ifs-stop MS` (0), `--ifs-step MS`
//! (0.5), `--gap zeros|noise` (what fills the gaps; zeros by default),
//! `--noise-from CF32` (recorded noise for `--gap noise`, else Gaussian at
//! -54.4 dBFS), `--retune-us N`, `--chunk SAMPLES` (emitted at once, as
//! a front end delivers them; 1024 by default), `--workers N` (runtime
//! threads), `--plugins DIR`, `--csv FILE`.

use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use anyhow::bail;
use futuresdr::futures::future::Either;
use futuresdr::futures::future::select;
use futuresdr::runtime::Runtime;
use futuresdr::runtime::Timer;
use futuresdr::runtime::dev::prelude::*;
use futuresdr::runtime::scheduler::SmolScheduler;
use plugin_api::BlockType;
use plugin_api::Plugin;
use plugin_api::add_kernel;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Hold;
use plugin_host::Registry;
use plugin_host::Standby;
use plugin_host::Tap;
use plugin_sdk::Sdk;

const RATE: f64 = 4e6;
/// Silence before the first frame, which the receivers need to start.
const LEAD_IN: Duration = Duration::from_millis(50);

/// The streams to replay, by step; the `Replay` block takes its `step`.
static STREAMS: Mutex<Vec<Arc<Vec<Complex32>>>> = Mutex::new(Vec::new());
/// When the replay of the current step emitted its first sample, so that a
/// frame can be attributed to the transmission it comes from.
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);
/// Samples the replay emits at once, as a front end delivers them.
static CHUNK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1024);

/// Emits a stream at `RATE` samples per second of wall clock, from its first
/// call on.
#[derive(Block)]
struct Replay {
    #[output]
    output: DefaultCpuWriter<Complex32>,
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
        let chunk = CHUNK.load(std::sync::atomic::Ordering::Relaxed);
        let len = self.samples.len();
        let due = ((start.elapsed().as_secs_f64() * RATE) as usize).min(len);
        // Whole chunks only: woken each time the reader takes some, a
        // block that emits what is due since would trade tiny pieces with
        // it, a core's worth of wake-ups.
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
        } else if n < ready {
            // The output is full; it calls again when there is room.
        } else {
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
    input: DefaultCpuReader<Complex32>,
    #[output]
    output: DefaultCpuWriter<Complex32>,
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
            Timer::after(self.delay).await;
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

fn replay_blocks() -> Plugin {
    Plugin::new(
        "replay",
        vec![
            BlockType {
                name: "Replay".into(),
                description: "A prepared stream, at 4 MSps.",
                add: |fg, s| {
                    let step: usize = s.get("step")?;
                    let samples = STREAMS.lock().unwrap()[step].clone();
                    add_kernel(
                        fg,
                        Replay {
                            output: DefaultCpuWriter::default(),
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
                            input: DefaultCpuReader::default(),
                            output: DefaultCpuWriter::default(),
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

fn read_cf32(path: &Path) -> Result<Vec<Complex32>> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            let [a, b, c, d, e, f, g, h] = *c;
            Complex32::new(
                f32::from_le_bytes([a, b, c, d]),
                f32::from_le_bytes([e, f, g, h]),
            )
        })
        .collect())
}

/// SplitMix64, for the noise.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn normal(&mut self) -> f32 {
        let mut uniform = || ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let (u, v) = (uniform(), uniform());
        ((-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()) as f32
    }
}

/// What fills the gaps.
enum Gap {
    Zeros,
    /// Quiet samples of a recording, taken in turn.
    Recorded(Vec<Complex32>, usize),
    /// Gaussian noise of this standard deviation per quadrature.
    Gaussian(Rng, f32),
}

impl Gap {
    fn fill(&mut self, out: &mut Vec<Complex32>, n: usize) {
        match self {
            Gap::Zeros => out.resize(out.len() + n, Complex32::default()),
            Gap::Recorded(noise, pos) => {
                for _ in 0..n {
                    out.push(noise[*pos]);
                    *pos = (*pos + 1) % noise.len();
                }
            }
            Gap::Gaussian(rng, sigma) => {
                out.extend((0..n).map(|_| Complex32::new(rng.normal(), rng.normal()) * *sigma));
            }
        }
    }
}

/// One transmitted frame: which PHY sent it, and when it began and ended,
/// in seconds from the first replayed sample.
#[derive(Debug, Clone, Copy)]
struct Sent {
    phy: usize,
    start: f64,
    end: f64,
}

/// `frames` frames, H Z H Z ..., `ifs` apart, after the lead-in, and what
/// was sent when.
fn stream(
    halow: &[Complex32],
    zigbee: &[Complex32],
    frames: usize,
    ifs: Duration,
    gap: &mut Gap,
) -> (Vec<Complex32>, Vec<Sent>) {
    let n_ifs = (ifs.as_secs_f64() * RATE).round() as usize;
    let n_lead = (LEAD_IN.as_secs_f64() * RATE) as usize;
    let mut out = Vec::new();
    let mut sent = Vec::new();
    gap.fill(&mut out, n_lead);
    for i in 0..frames {
        let phy = i % 2;
        let start = out.len();
        out.extend_from_slice(if phy == 0 { halow } else { zigbee });
        sent.push(Sent {
            phy,
            start: start as f64 / RATE,
            end: out.len() as f64 / RATE,
        });
        gap.fill(&mut out, if i + 1 == frames { n_lead } else { n_ifs });
    }
    (out, sent)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Select,
    Standby,
    Replace,
    Both,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Select => "select",
            Mode::Standby => "standby",
            Mode::Replace => "replace",
            Mode::Both => "both",
        }
    }

    /// Whether each PHY runs as a flowgraph of its own.
    fn two_flowgraphs(self) -> bool {
        matches!(self, Mode::Select | Mode::Both)
    }
}

/// The replay's flowgraph for step `step`.
fn head(step: usize, retune: Option<u64>) -> Result<Description> {
    let text = match retune {
        None => format!(
            r#"
            name = "replay"
            [blocks.replay]
            type = "Replay"
            step = {step}
            [outputs]
            samples = {{ port = "replay.output", type = "c32" }}
            "#
        ),
        Some(delay) => format!(
            r#"
            name = "replay"
            connections = "replay > tune"
            [blocks.replay]
            type = "Replay"
            step = {step}
            [blocks.tune]
            type = "Tune"
            delay_us = {delay}
            [outputs]
            samples = {{ port = "tune.output", type = "c32" }}
            [controls]
            frequency = "tune.freq"
            "#
        ),
    };
    Description::from_toml(&text)
}

/// Seconds of CPU time this process used.
fn cpu_time() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the command name, which is in parentheses.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map_or("", |(_, rest)| rest)
        .split_whitespace()
        .collect();
    let ticks = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    // utime and stime, fields 14 and 15, in clock ticks (100 per second).
    (ticks(11) + ticks(12)) / 100.0
}

struct StepResult {
    /// Frames from a receiver that was listening when they were sent.
    received: [usize; 2],
    /// Frames from a receiver that was listening for none of what it
    /// decoded: what this replay could deliver but a radio never would,
    /// since the transmitter had moved to the other PHY. Not counted as
    /// received.
    impossible: usize,
    /// How long after a frame was sent it was posted, in seconds.
    latency: Vec<f64>,
    switches: Vec<Duration>,
    cpu: f64,
    wall: Duration,
}

/// The next frame of `taps` and the PHY that posted it, or `None` after
/// `timeout`.
async fn next_frame(taps: &mut [Tap], timeout: Duration) -> Option<String> {
    let timer = Timer::after(timeout);
    let frame = async {
        match taps {
            [one] => one.recv_from().await,
            [a, b] => match select(pin!(a.recv_from()), pin!(b.recv_from())).await {
                Either::Left((m, _)) | Either::Right((m, _)) => m,
            },
            _ => unreachable!(),
        }
    };
    match select(pin!(frame), timer).await {
        Either::Left((Some((origin, Pmt::Blob(_))), _)) => Some(origin.to_string()),
        _ => None,
    }
}

/// Replay step `step` with the receivers `phys` (HaLow, ZigBee).
async fn run_step(
    ctrl: &mut Controller,
    mode: Mode,
    step: usize,
    retune: Option<u64>,
    phys: &[Description; 2],
    sent: &[Sent],
) -> Result<StepResult> {
    let names = ["halow", "zigbee"];
    let cpu = cpu_time();
    let t0 = Instant::now();
    *STARTED.lock().unwrap() = None;
    // The replay first: its stream ended in the previous step, and a
    // receiver that starts on an ended stream ends at once.
    ctrl.spawn_async("replay", head(step, retune)?).await?;
    let mut taps = Vec::new();
    let mut standby: Option<Standby> = None;
    if mode.two_flowgraphs() {
        for (name, desc) in names.iter().zip(phys) {
            taps.push(ctrl.tap(&format!("{name}.frames"))?);
            ctrl.spawn_async(name, desc.clone()).await?;
        }
        if mode == Mode::Select {
            ctrl.select_async("halow.samples", Hold::Discard).await?;
        }
    } else {
        taps.push(ctrl.tap("rx.frames")?);
        ctrl.spawn_async("rx", phys[0].clone()).await?;
        if mode == Mode::Standby {
            standby = Some(ctrl.prepare_async("rx", phys[1].clone()).await?);
        }
    }
    let mut received = [0usize; 2];
    let mut impossible = 0;
    let mut latency = Vec::new();
    // Which PHY the receivers were listening for, from a given time on.
    let mut listening: Vec<(f64, usize)> = vec![(0.0, 0)];
    let mut switches = Vec::new();
    let mut active = 0;
    let mut quiet_since: Option<Instant> = None;
    loop {
        let Some(origin) = next_frame(&mut taps, Duration::from_millis(20)).await else {
            let ended = ctrl
                .link_stats("replay.samples")
                .is_some_and(|s| s.closed && s.queued == 0);
            if ended
                && quiet_since.get_or_insert_with(Instant::now).elapsed()
                    > Duration::from_millis(200)
            {
                break;
            }
            continue;
        };
        quiet_since = None;
        let phy = names.iter().position(|n| *n == origin).unwrap();

        // Which frame this is: the last one of its PHY sent before it
        // arrived. Every copy is the same recording, so only the time it
        // was sent tells them apart.
        let at = STARTED
            .lock()
            .unwrap()
            .map_or(0.0, |start| start.elapsed().as_secs_f64());
        let sent_frame = sent.iter().rfind(|s| s.phy == phy && s.end <= at);
        // A receiver only ever gets what was sent while it was listening.
        // The recordings begin with a little silence, so listening for
        // part of a frame is enough; for none of it, a radio would have
        // given it nothing to decode.
        let listened = sent_frame.is_some_and(|f| {
            listening.iter().enumerate().any(|(k, (from, p))| {
                let until = listening.get(k + 1).map_or(f64::INFINITY, |(t, _)| *t);
                *p == phy && *from < f.end && until > f.start
            })
        });
        if let Some(f) = sent_frame {
            latency.push(at - f.end);
        }
        if mode == Mode::Both || listened {
            received[phy] += 1;
        } else {
            impossible += 1;
        }

        if phy != active || mode == Mode::Both {
            continue;
        }
        // Switch to the other PHY.
        let next = 1 - active;
        let t = Instant::now();
        match mode {
            Mode::Select => {
                ctrl.select_async(&format!("{}.samples", names[next]), Hold::Discard)
                    .await?;
            }
            Mode::Standby => {
                let ready = standby.take().expect("prepared");
                ctrl.commit_async(ready, Hold::Discard).await?;
            }
            Mode::Replace => {
                ctrl.replace_async("rx", phys[next].clone(), Hold::Discard)
                    .await?;
            }
            Mode::Both => unreachable!(),
        }
        switches.push(t.elapsed());
        active = next;
        let at = STARTED
            .lock()
            .unwrap()
            .map_or(0.0, |start| start.elapsed().as_secs_f64());
        listening.push((at, next));
        if mode == Mode::Standby {
            standby = Some(ctrl.prepare_async("rx", phys[active ^ 1].clone()).await?);
        }
    }
    let wall = t0.elapsed();
    let cpu = cpu_time() - cpu;

    drop(standby);
    ctrl.stop_async("replay").await?;
    let running: Vec<String> = ctrl.names().map(str::to_string).collect();
    for name in running {
        ctrl.stop_async(&name).await?;
    }
    Ok(StepResult {
        received,
        impossible,
        latency,
        switches,
        cpu,
        wall,
    })
}

/// Median of `v`, or zero.
fn median_of(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() -> Result<()> {
    let mut modes = vec![Mode::Select];
    let mut frames = 200;
    let (mut ifs_start, mut ifs_stop, mut ifs_step) = (4.0f64, 0.0f64, 0.5f64);
    let mut noise = false;
    let mut noise_from: Option<PathBuf> = None;
    let mut retune: Option<u64> = None;
    let mut plugins: Option<PathBuf> = None;
    let mut csv: Option<PathBuf> = None;
    let mut workers: Option<usize> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--mode", Some(v)) => {
                modes = match v.as_str() {
                    "select" => vec![Mode::Select],
                    "standby" => vec![Mode::Standby],
                    "replace" => vec![Mode::Replace],
                    "both" => vec![Mode::Both],
                    "all" => vec![Mode::Both, Mode::Select, Mode::Standby, Mode::Replace],
                    _ => bail!("unknown mode {v}"),
                }
            }
            ("--frames", Some(v)) => frames = v.parse::<usize>()?.max(2),
            ("--ifs-start", Some(v)) => ifs_start = v.parse()?,
            ("--ifs-stop", Some(v)) => ifs_stop = v.parse()?,
            ("--ifs-step", Some(v)) => ifs_step = v.parse::<f64>()?.abs(),
            ("--gap", Some(v)) if v == "zeros" => noise = false,
            ("--gap", Some(v)) if v == "noise" => noise = true,
            ("--noise-from", Some(v)) => noise_from = Some(v.into()),
            ("--retune-us", Some(v)) => retune = Some(v.parse()?),
            ("--chunk", Some(v)) => CHUNK.store(
                v.parse::<usize>()?.max(1),
                std::sync::atomic::Ordering::Relaxed,
            ),
            ("--workers", Some(v)) => workers = Some(v.parse()?),
            ("--plugins", Some(v)) => plugins = Some(v.into()),
            ("--csv", Some(v)) => csv = Some(v.into()),
            _ => bail!(
                "usage: ziglow_replay [--mode select|standby|replace|both|all] [--frames N] \
                 [--ifs-start MS] [--ifs-stop MS] [--ifs-step MS] [--gap zeros|noise] \
                 [--noise-from CF32] [--retune-us N] [--chunk SAMPLES] [--workers N] \
                 [--plugins DIR] [--csv FILE]"
            ),
        }
    }
    if ifs_step == 0.0 {
        bail!("--ifs-step must not be 0");
    }

    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = here.join("..");
    let mut registry = Registry::new();
    match plugins {
        Some(dir) => {
            registry.load_dir(&dir)?;
        }
        None => {
            let sdk = Sdk::of_this_process()?;
            for plugin in ["basic", "wlan", "zigbee"] {
                let library = sdk.build_plugin(
                    &workspace.join(format!("blocks/{plugin}/Cargo.toml")),
                    &workspace.join("target/plugins"),
                )?;
                registry.load(&library)?;
            }
        }
    }
    registry.register(replay_blocks())?;

    // The streams, one per IFS.
    let testdata = workspace.join("blocks/testdata");
    let halow = read_cf32(&testdata.join("halow_frame.cf32"))?;
    let zigbee = read_cf32(&testdata.join("zigbee_frame.cf32"))?;
    let mut gap = match (noise, noise_from) {
        (false, _) => Gap::Zeros,
        (true, Some(path)) => {
            // The quietest 40 %, in order: noise between bursts.
            let all = read_cf32(&path)?;
            let mut power: Vec<f32> = all.iter().map(|x| x.norm_sqr()).collect();
            power.sort_by(f32::total_cmp);
            let limit = power[power.len() * 2 / 5];
            let quiet: Vec<Complex32> = all.into_iter().filter(|x| x.norm_sqr() <= limit).collect();
            Gap::Recorded(quiet, 0)
        }
        (true, None) => Gap::Gaussian(Rng(1), 10f32.powf(-54.4 / 20.0) / 2f32.sqrt()),
    };
    let mut sent_per_step: Vec<Vec<Sent>> = Vec::new();
    let mut ifs = Vec::new();
    let mut v = ifs_start;
    while (ifs_step > 0.0 && ifs_start >= ifs_stop && v >= ifs_stop - 1e-9)
        || (ifs_start < ifs_stop && v <= ifs_stop + 1e-9)
    {
        ifs.push(v.max(0.0));
        v += if ifs_start >= ifs_stop {
            -ifs_step
        } else {
            ifs_step
        };
    }
    {
        let mut streams = STREAMS.lock().unwrap();
        for ms in &ifs {
            let ifs = Duration::from_secs_f64(ms / 1e3);
            let (samples, manifest) = stream(&halow, &zigbee, frames, ifs, &mut gap);
            streams.push(Arc::new(samples));
            sent_per_step.push(manifest);
        }
    }
    let phys = [
        Description::from_file(here.join("examples/flows/replay/halow.toml"))?,
        Description::from_file(here.join("examples/flows/replay/zigbee.toml"))?,
    ];
    let sent_h = frames.div_ceil(2);
    let sent_z = frames / 2;
    println!(
        "{frames} frames per IFS ({sent_h} H, {sent_z} Z), chunks of {} samples, gaps of {}, {}",
        CHUNK.load(std::sync::atomic::Ordering::Relaxed),
        if noise { "noise" } else { "zeros" },
        match retune {
            Some(us) => format!("retuning in {us} µs"),
            None => "no retuning".into(),
        }
    );

    let mut table = String::from(
        "mode,ifs_ms,sent_h,rx_h,sent_z,rx_z,per_h,per_z,impossible,switches,\
         switch_median_ms,switch_max_ms,decode_median_ms,cpu_ms,wall_ms\n",
    );
    for mode in modes {
        println!("\n{}", mode.name());
        println!(
            "  IFS ms   PER H    PER Z   switch median / max ms   decode ms   CPU ms   impossible"
        );
        // A controller per mode, with the links it uses.
        let mut ctrl = match workers {
            Some(n) => Controller::with_runtime(
                Runtime::with_scheduler(SmolScheduler::with_config(n, false)),
                registry.clone(),
            ),
            None => Controller::new(registry.clone()),
        };
        if mode.two_flowgraphs() {
            ctrl.link("replay.samples", "halow.samples")?;
            ctrl.link("replay.samples", "zigbee.samples")?;
        } else {
            ctrl.link("replay.samples", "rx.samples")?;
        }
        let ifs = ifs.clone();
        let phys = phys.clone();
        let sent = sent_per_step.clone();
        let rows = ctrl.run(move |mut ctrl| async move {
            let mut rows = Vec::new();
            for (step, ms) in ifs.iter().enumerate() {
                let r = run_step(&mut ctrl, mode, step, retune, &phys, &sent[step]).await?;
                rows.push((*ms, r));
            }
            anyhow::Ok(rows)
        })?;
        for (ifs, r) in rows {
            let per_h = 1.0 - r.received[0] as f64 / sent_h as f64;
            let per_z = 1.0 - r.received[1] as f64 / sent_z as f64;
            let mut sw: Vec<f64> = r.switches.iter().map(|d| ms(*d)).collect();
            sw.sort_by(f64::total_cmp);
            let median = sw.get(sw.len() / 2).copied().unwrap_or(0.0);
            let max = sw.last().copied().unwrap_or(0.0);
            println!(
                "  {ifs:6.2}  {:6.1}%  {:6.1}%   {median:8.3} / {max:8.3}   {:9.3}   {:7.0}   \
                 {:10}",
                100.0 * per_h,
                100.0 * per_z,
                1e3 * median_of(&r.latency),
                r.cpu * 1e3,
                r.impossible,
            );
            writeln!(
                table,
                "{},{ifs},{sent_h},{},{sent_z},{},{per_h:.4},{per_z:.4},{},{},{median:.4},\
                 {max:.4},{:.4},{:.0},{:.0}",
                mode.name(),
                r.received[0],
                r.received[1],
                r.impossible,
                sw.len(),
                1e3 * median_of(&r.latency),
                r.cpu * 1e3,
                ms(r.wall)
            )?;
        }
    }
    if let Some(path) = csv {
        std::fs::write(&path, table)?;
        println!("\nwrote {}", path.display());
    }
    Ok(())
}
