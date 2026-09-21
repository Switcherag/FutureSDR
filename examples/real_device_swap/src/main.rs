//! A receiver that changes PHY after every frame, on a real radio: the dyn
//! branch's `real_device_swap` (`ziglow_swap_quicktune`), on the plugin
//! controller.
//!
//! ```text
//! radio ──samples──▶ rx: HaLow (802.11ah, 919 MHz) ⇄ ZigBee (802.15.4, 2.425 GHz)
//! ```
//!
//! Each time the receiver posts a frame, it is replaced by the other PHY
//! (`Controller::replace`: in a real exchange, what comes next is announced
//! in the frame, so the receiver changes on demand). The receivers ask for
//! their channel in their `[radio]` sections, and the controller sets it on
//! the radio before the new receiver gets samples.
//!
//! Sources (`--source`):
//!
//! - `bladerf` (feature `bladerf`, on by default): a bladeRF 2.0 through
//!   libbladeRF, retuned by quick tune (`--no-quick-tune` for
//!   `set_frequency`). Runs until `--duration` or `--frames`, and writes a
//!   row per frame: the PHY, and for ZigBee frames of the dyn branch's
//!   multizig transmitter the step, run and programmed inter-frame spacing
//!   (IFS) from the frame's stamp; for HaLow frames the 802.11 sequence
//!   number.
//! - `replay`: recorded frames, H Z H Z ..., in place of the radio, for
//!   testing without one. The IFS after each H frame, the time the receiver
//!   has to change from HaLow to ZigBee, is swept (`--ifs-h2z`); the IFS
//!   after each Z frame is fixed (`--ifs-z2h`). The replayed front end takes
//!   `--retune-us` to change channel (300 µs, quick tune's). Writes a row
//!   per IFS with the packet error rate of each PHY.
//!
//! ```text
//! cd examples/real_device_swap
//! cargo run --release -- --source replay
//! cargo run --release -- --source bladerf --duration 60
//! ```

// FutureSDR as the plugins see it; `#[derive(Block)]` names it `futuresdr`.
extern crate futuresdr_plugin_rt as futuresdr;

#[cfg(feature = "bladerf")]
mod bladerf_source;
mod replay;

use std::fmt::Write as _;
#[cfg(feature = "bladerf")]
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::pin::pin;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use clap::ValueEnum;
use futuresdr::futures::future::Either;
use futuresdr::futures::future::select;
use futuresdr::runtime::Pmt;
use futuresdr::runtime::Runtime;
use futuresdr::runtime::Timer;
use futuresdr::runtime::scheduler::SmolScheduler;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Hold;
use plugin_host::Registry;
use plugin_host::Tap;
use plugin_sdk::Sdk;

/// The PHYs, in the order of the replay's frames.
const PHYS: [&str; 2] = ["halow", "zigbee"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Source {
    Bladerf,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Phy {
    Halow,
    Zigbee,
}

#[derive(Parser, Debug)]
#[command(about = "A receiver that changes PHY (802.11ah / 802.15.4) after every frame.")]
struct Args {
    /// Where the samples come from.
    #[arg(long, value_enum, default_value = "bladerf")]
    source: Source,
    /// The PHY listened for first.
    #[arg(long, value_enum)]
    first: Option<Phy>,
    /// Plugin libraries to load, instead of building crates/plugin/blocks.
    #[arg(long)]
    plugins: Option<PathBuf>,
    /// Runtime threads (one per core by default).
    #[arg(long)]
    workers: Option<usize>,
    /// The HaLow receiver's description, relative to flows/: halow.toml
    /// (fused blocks) or wlan_granular.toml (examples/wlan's blocks).
    #[arg(long, default_value = "halow.toml")]
    halow: String,
    /// Output CSV (default: real_device_swap.csv or real_device_replay.csv).
    #[arg(long)]
    csv: Option<PathBuf>,

    // bladeRF
    /// Hardware sample rate, S/s.
    #[arg(long, default_value_t = 20e6)]
    sample_rate: f64,
    /// Decimation to the receivers' 4 MSps.
    #[arg(long, default_value_t = 5)]
    decim: usize,
    #[arg(long, default_value_t = 10)]
    gain_db: i32,
    /// Samples per USB transfer (a multiple of 1024).
    #[arg(long, default_value_t = 4096)]
    stream_buffer: usize,
    #[arg(long, default_value_t = 16)]
    stream_buffers: u32,
    #[arg(long, default_value_t = 8)]
    stream_transfers: u32,
    /// Retune with set_frequency instead of quick tune.
    #[arg(long)]
    no_quick_tune: bool,
    /// Microseconds of samples dropped after each retune.
    #[arg(long, default_value_t = 0)]
    drop_after_retune_us: u64,
    /// Register the quick-tune profiles, report them and exit.
    #[arg(long)]
    register_only: bool,
    /// Stop after this many seconds.
    #[arg(long, default_value_t = 60.0)]
    duration: f64,
    /// Stop after this many frames (0: no limit).
    #[arg(long, default_value_t = 0)]
    frames: usize,
    /// Change PHY anyway after this long without a frame, ms.
    #[arg(long, default_value_t = 80_000)]
    rx_timeout_ms: u64,

    // replay
    /// IFS after each H frame, ms: `START:STOP:STEP` or a list `1,0.5,0.2`.
    #[arg(long, default_value = "1:0:0.1")]
    ifs_h2z: String,
    /// IFS after each Z frame, ms.
    #[arg(long, default_value_t = 1.0)]
    ifs_z2h: f64,
    /// Frames per IFS (H and Z together).
    #[arg(long, default_value_t = 100)]
    frames_per_step: usize,
    /// Time the replayed front end takes to retune, µs.
    #[arg(long, default_value_t = 300)]
    retune_us: u64,
    /// Samples the replay delivers at once.
    #[arg(long, default_value_t = 256)]
    chunk: usize,
}

/// `START:STOP:STEP` or `a,b,c`, in ms.
fn parse_sweep(spec: &str) -> Result<Vec<f64>> {
    let parts: Vec<&str> = spec.split(':').collect();
    if let [start, stop, step] = parts[..] {
        let (start, stop, step): (f64, f64, f64) = (start.parse()?, stop.parse()?, step.parse()?);
        if step <= 0.0 {
            bail!("the step of --ifs-h2z must be positive");
        }
        let n = ((start - stop).abs() / step + 1e-9).floor() as usize;
        let dir = if stop < start { -1.0 } else { 1.0 };
        return Ok((0..=n)
            .map(|k| (start + dir * step * k as f64).max(0.0))
            .collect());
    }
    spec.split(',')
        .map(|v| Ok(v.trim().parse::<f64>()?))
        .collect()
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/plugin")
}

fn registry(args: &Args) -> Result<Registry> {
    let mut registry = Registry::new();
    match &args.plugins {
        Some(dir) => {
            registry.load_dir(dir)?;
        }
        None => {
            // Built against the shared library this program runs, once.
            let sdk = Sdk::of_this_process()?;
            let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/plugins");
            for plugin in ["basic", "wlan", "zigbee"] {
                let manifest = workspace().join(format!("blocks/{plugin}/Cargo.toml"));
                registry.load(&sdk.build_plugin(&manifest, &target)?)?;
            }
        }
    }
    Ok(registry)
}

fn receivers(halow: &str) -> Result<[Description; 2]> {
    let flows = Path::new(env!("CARGO_MANIFEST_DIR")).join("flows");
    Ok([
        Description::from_file(flows.join(halow))?,
        Description::from_file(flows.join("zigbee.toml"))?,
    ])
}

fn controller(args: &Args, registry: Registry) -> Controller {
    match args.workers {
        Some(n) => Controller::with_runtime(
            Runtime::with_scheduler(SmolScheduler::with_config(n, false)),
            registry,
        ),
        None => Controller::new(registry),
    }
}

/// The next frame of `tap` and the description that posted it, or `None`
/// after `timeout`.
async fn next_frame(tap: &mut Tap, timeout: Duration) -> Option<(String, Vec<u8>)> {
    match select(pin!(tap.recv_from()), Timer::after(timeout)).await {
        Either::Left((Some((origin, Pmt::Blob(frame))), _)) => Some((origin.to_string(), frame)),
        _ => None,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

// ── Frames ───────────────────────────────────────────────────────────────

/// Source address of the dyn branch's multizig transmitter, `00 00 'E' 'E'
/// 'B' 'G' 'I' 'Z'`, just before the stamp in its frames.
#[cfg_attr(not(feature = "bladerf"), allow(dead_code))]
const ZIGBEE_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// The 19-byte stamp of a multizig frame, little-endian: frame number in
/// the step (u32), step (u16), tag (u8), programmed IFS in µs (u32), and
/// the transmitter's clock in µs (u64).
#[cfg_attr(not(feature = "bladerf"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
struct Stamp {
    frame: u32,
    step: u16,
    tag: u8,
    ifs_us: u32,
    ts_us: u64,
}

#[cfg_attr(not(feature = "bladerf"), allow(dead_code))]
fn parse_stamp(frame: &[u8]) -> Option<Stamp> {
    let needed = ZIGBEE_ANCHOR.len() + 19;
    let at = frame
        .windows(needed)
        .position(|w| w[..ZIGBEE_ANCHOR.len()] == ZIGBEE_ANCHOR)?
        + ZIGBEE_ANCHOR.len();
    let p = &frame[at..at + 19];
    Some(Stamp {
        frame: u32::from_le_bytes(p[0..4].try_into().unwrap()),
        step: u16::from_le_bytes(p[4..6].try_into().unwrap()),
        tag: p[6],
        ifs_us: u32::from_le_bytes(p[7..11].try_into().unwrap()),
        ts_us: u64::from_le_bytes(p[11..19].try_into().unwrap()),
    })
}

/// The sequence number of an 802.11 MAC frame; control frames have none.
#[cfg_attr(not(feature = "bladerf"), allow(dead_code))]
fn parse_seq(frame: &[u8]) -> Option<u16> {
    if frame.len() < 24 || (frame[0] >> 2) & 0x3 == 1 {
        return None;
    }
    Some(u16::from_le_bytes([frame[22], frame[23]]) >> 4)
}

// ── bladeRF ──────────────────────────────────────────────────────────────

#[cfg(feature = "bladerf")]
fn run_bladerf(args: &Args, registry: Registry) -> Result<()> {
    use std::sync::Arc;

    use bladerf_source::BladeRfSource;
    use bladerf_source::Radio;
    use plugin_api::BlockType;
    use plugin_api::Plugin;
    use plugin_api::add_kernel;

    let phys = receivers(&args.halow)?;
    let channels: Vec<u64> = phys
        .iter()
        .map(|d| {
            d.radio
                .iter()
                .find(|(k, _)| k == "frequency")
                .and_then(|(_, v)| match v {
                    Pmt::F64(hz) => Some(*hz),
                    Pmt::U64(hz) => Some(*hz as f64),
                    Pmt::Isize(hz) => Some(*hz as f64),
                    _ => None,
                })
                .map(|hz| hz.round() as u64)
                .ok_or_else(|| anyhow::anyhow!("{:?} has no [radio] frequency", d.name))
        })
        .collect::<Result<_>>()?;
    let rate = args.sample_rate / args.decim.max(1) as f64;
    if (rate - replay::RATE).abs() > 1.0 {
        bail!(
            "--sample-rate / --decim is {rate} S/s; the receivers expect {}",
            replay::RATE
        );
    }
    let (radio, profiles) = Radio::open(bladerf_source::Settings {
        sample_rate: args.sample_rate as u32,
        decim: args.decim,
        gain_db: args.gain_db,
        buffer: args.stream_buffer,
        buffers: args.stream_buffers,
        transfers: args.stream_transfers,
        channels,
        no_quick_tune: args.no_quick_tune,
        drop_after_retune: (args.drop_after_retune_us as f64 * 1e-6 * rate) as usize,
    })?;
    println!("bladeRF {}", radio.serial());
    for p in &profiles {
        println!("  quick tune {p}");
    }
    if args.register_only {
        return Ok(());
    }

    // The radio as a block type, for the radio flowgraph's description.
    static RADIO: std::sync::Mutex<Option<Arc<Radio>>> = std::sync::Mutex::new(None);
    *RADIO.lock().unwrap() = Some(radio.clone());
    let mut registry = registry;
    registry.register(Plugin::new(
        "bladerf",
        vec![BlockType {
            name: "BladeRfSource".into(),
            description: "The bladeRF, decimated to 4 MSps; message inputs freq, gain.",
            add: |fg, _s| {
                let radio = RADIO.lock().unwrap().clone().expect("opened");
                add_kernel(fg, BladeRfSource::new(radio))
            },
        }],
    ))?;
    let head = Description::from_toml(
        r#"
        name = "radio"
        [blocks.src]
        type = "BladeRfSource"
        [outputs]
        samples = { port = "src.output", type = "c32" }
        [controls]
        frequency = "src.freq"
        gain = "src.gain"
        "#,
    )?;

    let csv_path = args
        .csv
        .clone()
        .unwrap_or_else(|| "real_device_swap.csv".into());
    let mut csv = std::fs::File::create(&csv_path)?;
    writeln!(
        csv,
        "rx_idx,phy,event,len,frame,step,tag,ifs_us,ts_us,seq,rx_t_ms,swap_ms,retune_ms"
    )?;
    let first = match args.first.unwrap_or(Phy::Zigbee) {
        Phy::Halow => 0,
        Phy::Zigbee => 1,
    };
    let (duration, max_frames) = (Duration::from_secs_f64(args.duration), args.frames);
    let timeout = Duration::from_millis(args.rx_timeout_ms);

    let mut ctrl = controller(args, registry);
    ctrl.link("radio.samples", "rx.samples")?;
    let (swaps, retunes, counts) = ctrl.run(move |mut ctrl| async move {
        ctrl.spawn_async("radio", head).await?;
        let mut tap = ctrl.tap("rx.frames")?;
        ctrl.spawn_async("rx", phys[first].clone()).await?;
        println!("listening for {} first; a frame changes PHY", PHYS[first]);

        let t0 = Instant::now();
        let (mut active, mut rx_idx) = (first, 0u64);
        let (mut swaps, mut retunes, mut counts) = (Vec::new(), Vec::new(), [0usize; 2]);
        while t0.elapsed() < duration && (max_frames == 0 || rx_idx < max_frames as u64) {
            let left = duration.saturating_sub(t0.elapsed()).min(timeout);
            let frame = next_frame(&mut tap, left).await;
            let rx_t = ms(t0.elapsed());
            if frame.is_none() && t0.elapsed() >= duration {
                break;
            }
            let t = Instant::now();
            let replacement = ctrl
                .replace_async("rx", phys[1 - active].clone(), Hold::Discard)
                .await?;
            let swap = ms(t.elapsed());
            let retune = ms(replacement.timings.controls);
            active = 1 - active;
            swaps.push(swap);
            retunes.push(retune);

            let row = match &frame {
                None => format!("{rx_idx},?,timeout,-1,-1,-1,-1,-1,-1,-1"),
                Some((origin, bytes)) => {
                    let phy = PHYS.iter().position(|p| p == origin).unwrap_or(0);
                    counts[phy] += 1;
                    let (stamp, seq) = if phy == 1 {
                        (parse_stamp(bytes), None)
                    } else {
                        (None, parse_seq(bytes))
                    };
                    let s = |v: Option<String>| v.unwrap_or_else(|| "-1".into());
                    format!(
                        "{rx_idx},{},rx,{},{},{},{},{},{},{}",
                        if phy == 0 { "H" } else { "Z" },
                        bytes.len(),
                        s(stamp.map(|x| x.frame.to_string())),
                        s(stamp.map(|x| x.step.to_string())),
                        s(stamp.map(|x| x.tag.to_string())),
                        s(stamp.map(|x| x.ifs_us.to_string())),
                        s(stamp.map(|x| x.ts_us.to_string())),
                        s(seq.map(|x| x.to_string())),
                    )
                }
            };
            writeln!(csv, "{row},{rx_t:.3},{swap:.3},{retune:.3}")?;
            csv.flush().ok();
            if let Some((origin, bytes)) = &frame {
                println!(
                    "[{rx_t:9.1} ms] {origin:6} {:4} B   swap {swap:.3} ms (retune {retune:.3})",
                    bytes.len()
                );
            }
            rx_idx += 1;
        }
        let names: Vec<String> = ctrl.names().map(str::to_string).collect();
        for name in names {
            ctrl.stop_async(&name).await?;
        }
        anyhow::Ok((swaps, retunes, counts))
    })?;
    let (mut swaps, mut retunes) = (swaps, retunes);
    println!(
        "\n{} frames (H {}, Z {}), {} swaps: median {:.3} ms, retune median {:.3} ms; \
         quick-tune misses {}",
        counts[0] + counts[1],
        counts[0],
        counts[1],
        swaps.len(),
        median(&mut swaps),
        median(&mut retunes),
        radio.misses.load(std::sync::atomic::Ordering::Relaxed),
    );
    println!("wrote {}", csv_path.display());
    Ok(())
}

#[cfg(not(feature = "bladerf"))]
fn run_bladerf(_args: &Args, _registry: Registry) -> Result<()> {
    bail!("built without the `bladerf` feature; use --source replay")
}

// ── Replay ───────────────────────────────────────────────────────────────

/// What one IFS of the replay gave.
struct StepResult {
    /// Frames from a receiver that was listening when they were sent.
    received: [usize; 2],
    /// Frames a radio could not have given: its receiver was listening for
    /// none of the transmission it decoded. Not counted as received.
    impossible: usize,
    swaps: Vec<f64>,
    retunes: Vec<f64>,
}

async fn replay_step(
    ctrl: &mut Controller,
    step: usize,
    retune_us: u64,
    phys: &[Description; 2],
    sent: &[replay::Sent],
    first: usize,
) -> Result<StepResult> {
    *replay::STARTED.lock().unwrap() = None;
    // The radio first: a receiver that starts on an ended stream ends.
    ctrl.spawn_async(
        "radio",
        Description::from_toml(&replay::head(step, retune_us))?,
    )
    .await?;
    let mut tap = ctrl.tap("rx.frames")?;
    ctrl.spawn_async("rx", phys[first].clone()).await?;

    let mut received = [0usize; 2];
    let mut impossible = 0;
    let (mut swaps, mut retunes) = (Vec::new(), Vec::new());
    // Which PHY the receiver listened for, from a given time on.
    let mut listening: Vec<(f64, usize)> = vec![(0.0, first)];
    let mut active = first;
    let mut quiet_since: Option<Instant> = None;
    loop {
        let Some((origin, _)) = next_frame(&mut tap, Duration::from_millis(20)).await else {
            let ended = ctrl
                .link_stats("radio.samples")
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
        let phy = PHYS.iter().position(|p| *p == origin).unwrap();

        // The transmission it decodes: the last of its PHY that ended before
        // it arrived (every copy is the same recording).
        let at = replay::now();
        let frame = sent.iter().rfind(|s| s.phy == phy && s.end <= at);
        let listened = frame.is_some_and(|f| {
            listening.iter().enumerate().any(|(k, (from, p))| {
                let until = listening.get(k + 1).map_or(f64::INFINITY, |(t, _)| *t);
                *p == phy && *from < f.end && until > f.start
            })
        });
        if listened {
            received[phy] += 1;
        } else {
            impossible += 1;
        }
        if phy != active {
            continue;
        }
        let next = 1 - active;
        let t = Instant::now();
        let replacement = ctrl
            .replace_async("rx", phys[next].clone(), Hold::Discard)
            .await?;
        swaps.push(ms(t.elapsed()));
        retunes.push(ms(replacement.timings.controls));
        active = next;
        listening.push((replay::now(), next));
    }
    let names: Vec<String> = ctrl.names().map(str::to_string).collect();
    for name in names {
        ctrl.stop_async(&name).await?;
    }
    Ok(StepResult {
        received,
        impossible,
        swaps,
        retunes,
    })
}

fn run_replay(args: &Args, mut registry: Registry) -> Result<()> {
    registry.register(replay::blocks())?;
    replay::CHUNK.store(args.chunk.max(1), std::sync::atomic::Ordering::Relaxed);
    let testdata = workspace().join("blocks/testdata");
    let halow = replay::read_cf32(&testdata.join("halow_frame.cf32"))?;
    let zigbee = replay::read_cf32(&testdata.join("zigbee_frame.cf32"))?;
    let frames = args.frames_per_step.max(2);
    let steps = replay::prepare(
        &halow,
        &zigbee,
        frames,
        parse_sweep(&args.ifs_h2z)?,
        args.ifs_z2h,
    );
    if args.first == Some(Phy::Zigbee) {
        bail!("the replay starts with a HaLow frame; --first zigbee does not apply");
    }
    let (sent_h, sent_z) = (frames.div_ceil(2), frames / 2);
    println!(
        "{frames} frames per IFS ({sent_h} H of {:.0} µs, {sent_z} Z of {:.0} µs), \
         IFS after Z {} ms, retune {} µs, chunks of {} samples",
        halow.len() as f64 / replay::RATE * 1e6,
        zigbee.len() as f64 / replay::RATE * 1e6,
        steps.ifs_z2h_ms,
        args.retune_us,
        args.chunk,
    );
    println!("  IFS H→Z ms   PER H    PER Z   swap median / max ms   retune ms   impossible");

    let phys = receivers(&args.halow)?;
    let mut ctrl = controller(args, registry);
    ctrl.link("radio.samples", "rx.samples")?;
    let retune_us = args.retune_us;
    let sent = steps.sent.clone();
    let n_steps = steps.ifs_h2z_ms.len();
    let results = ctrl.run(move |mut ctrl| async move {
        let mut results = Vec::new();
        for (step, sent) in sent.iter().enumerate().take(n_steps) {
            results.push(replay_step(&mut ctrl, step, retune_us, &phys, sent, 0).await?);
        }
        anyhow::Ok(results)
    })?;

    let mut table = String::from(
        "ifs_h2z_ms,ifs_z2h_ms,sent_h,rx_h,sent_z,rx_z,per_h,per_z,impossible,swaps,\
         swap_median_ms,swap_max_ms,retune_median_ms\n",
    );
    for (ifs, mut r) in steps.ifs_h2z_ms.iter().zip(results) {
        let per_h = 1.0 - r.received[0] as f64 / sent_h as f64;
        let per_z = 1.0 - r.received[1] as f64 / sent_z as f64;
        let n = r.swaps.len();
        let swap = median(&mut r.swaps);
        let max = r.swaps.last().copied().unwrap_or(f64::NAN);
        let retune = median(&mut r.retunes);
        println!(
            "  {ifs:9.2}  {:6.1}%  {:6.1}%   {swap:8.3} / {max:8.3}   {retune:9.3}   {:10}",
            100.0 * per_h,
            100.0 * per_z,
            r.impossible,
        );
        writeln!(
            table,
            "{ifs},{},{sent_h},{},{sent_z},{},{per_h:.4},{per_z:.4},{},{n},{swap:.4},{max:.4},\
             {retune:.4}",
            steps.ifs_z2h_ms, r.received[0], r.received[1], r.impossible,
        )?;
    }
    let path = args
        .csv
        .clone()
        .unwrap_or_else(|| "real_device_replay.csv".into());
    std::fs::write(&path, table)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let registry = registry(&args)?;
    match args.source {
        Source::Bladerf => run_bladerf(&args, registry),
        Source::Replay => run_replay(&args, registry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps() {
        assert_eq!(parse_sweep("1:0:0.5").unwrap(), vec![1.0, 0.5, 0.0]);
        assert_eq!(parse_sweep("0:0.2:0.1").unwrap().len(), 3);
        assert_eq!(parse_sweep("0.3, 0.1").unwrap(), vec![0.3, 0.1]);
        assert!(parse_sweep("1:0:0").is_err());
    }

    #[test]
    fn a_multizig_stamp_is_read() {
        let mut frame = vec![0x41, 0x88, 7, 0xff, 0xff];
        frame.extend_from_slice(&ZIGBEE_ANCHOR);
        frame.extend_from_slice(&5u32.to_le_bytes());
        frame.extend_from_slice(&2u16.to_le_bytes());
        frame.push(0x0f);
        frame.extend_from_slice(&1500u32.to_le_bytes());
        frame.extend_from_slice(&123_456u64.to_le_bytes());
        frame.extend_from_slice(&[0, 0]);
        let s = parse_stamp(&frame).unwrap();
        assert_eq!(
            (s.frame, s.step, s.tag, s.ifs_us, s.ts_us),
            (5, 2, 15, 1500, 123_456)
        );
        assert!(parse_stamp(&frame[..20]).is_none());
    }

    #[test]
    fn an_80211_sequence_number_is_read() {
        let mut frame = vec![0u8; 30];
        frame[0] = 0x08; // data
        frame[22..24].copy_from_slice(&(0x123u16 << 4).to_le_bytes());
        assert_eq!(parse_seq(&frame), Some(0x123));
        frame[0] = 0x04; // type 1: control
        assert_eq!(parse_seq(&frame), None);
    }
}
