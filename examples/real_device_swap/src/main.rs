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

use std::collections::BTreeMap;
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
    /// Runtime threads (one per core by default; one per CPU of --cpus).
    #[arg(long)]
    workers: Option<usize>,
    /// Run on these CPUs only, a runtime thread pinned to each: a list, e.g.
    /// `8,10,2,4`, of CPUs on different physical cores (see `lscpu -e`: two
    /// hardware threads of one core share it), or `auto`: one CPU of each of
    /// the fastest physical cores, as many as --workers (4 by default). On a
    /// hybrid Intel CPU that is performance cores; on a Raspberry Pi 5, CPUs
    /// 0 to 3.
    #[arg(long)]
    cpus: Option<String>,
    /// The HaLow receiver's description, relative to flows/:
    /// wlan_simple.toml (the wlan plugin's fused blocks) or
    /// wlan_granular.toml (examples/wlan's blocks, one by one). With the
    /// replay, several separated by commas are run one after the other and
    /// compared.
    #[arg(long, default_value = "wlan_simple.toml")]
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
    /// With the radio: run until Enter is pressed (for scripts that wait for
    /// the transmitter), --duration and --frames still apply.
    #[arg(long)]
    until_enter: bool,
    /// With the radio: print every frame, not a line every 5 s.
    #[arg(long)]
    verbose: bool,
    /// Change PHY anyway after this long without a frame, ms.
    #[arg(long, default_value_t = 80_000)]
    rx_timeout_ms: u64,

    // replay
    /// The two receivers that take turns, descriptions in flows/, e.g.
    /// `zigbee.toml,zigbee.toml` for the same PHY replaced by itself. The
    /// replay sends their PHYs' frames in turn. Default: `--halow` and
    /// zigbee.toml.
    #[arg(long)]
    swap: Option<String>,
    /// IFS after each frame for the first receiver (with the default pair,
    /// each H frame), ms: `START:STOP:STEP` or a list `1,0.5,0.2`.
    #[arg(long, alias = "ifs-h2z", default_value = "1:0:0.1")]
    ifs: String,
    /// IFS after each frame for the second receiver, ms; the swept IFS if
    /// not given.
    #[arg(long, alias = "ifs-z2h")]
    ifs_after_second: Option<f64>,
    /// Frames per IFS (both receivers' together).
    #[arg(long, default_value_t = 100)]
    frames_per_step: usize,
    /// Time the replayed front end takes to retune, µs.
    #[arg(long, default_value_t = 300)]
    retune_us: u64,
    /// Replay the recordings whole, silence around the frames included, as
    /// the dyn branch's generator does; the IFS is then the gap between
    /// recordings, not between frames.
    #[arg(long)]
    no_trim: bool,
    /// Real-time priority (SCHED_FIFO, 1 to 99) for the runtime's threads
    /// and the source's, so that other processes cannot hold their CPUs; it
    /// needs the right to (rtprio limit or CAP_SYS_NICE).
    #[arg(long)]
    rt_priority: Option<usize>,
    /// Keep the CPUs of --cpus awake: a thread spinning at the lowest
    /// priority on each, so that they neither sleep in deep idle states
    /// (C3 takes about 1 ms to leave here) nor slow down their clocks. Costs
    /// their power.
    #[arg(long)]
    keep_awake: bool,
    /// Keep the first receiver for the whole replay (both of the pair must
    /// be of one PHY): the reference, without swaps.
    #[arg(long)]
    no_swap: bool,
    /// Samples the replay's output holds before it drops what it cannot
    /// deliver, as a radio's buffers do: 13107 is what this program's
    /// bladeRF setup buffers (16 transfers of 4096 samples at 20 MSps,
    /// 3.3 ms), at 4 MSps.
    #[arg(long, default_value_t = 13_107)]
    source_buffer: usize,
    /// Samples a link between flowgraphs holds before it drops the oldest,
    /// as a radio's buffers do: 16384 is 4 ms at 4 MSps, about what a
    /// bladeRF buffers (16 transfers of 4096 samples at 20 MSps).
    #[arg(long, default_value_t = 16384)]
    link_capacity: usize,
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

fn controller(args: &Args, cpus: &[usize], registry: Registry) -> Controller {
    let mut ctrl = controller_on(args, cpus, registry);
    ctrl.set_link_capacity(args.link_capacity);
    ctrl
}

fn controller_on(args: &Args, cpus: &[usize], registry: Registry) -> Controller {
    let pinned = !cpus.is_empty();
    match args.workers.or(pinned.then_some(cpus.len())) {
        // Pinned in turn to the CPUs the process may use: those of --cpus.
        Some(n) => Controller::with_runtime(
            Runtime::with_scheduler(SmolScheduler::with_config(n, pinned)),
            registry,
        ),
        None => Controller::new(registry),
    }
}

/// The CPUs `--cpus` names.
fn parse_cpus(spec: &str, workers: Option<usize>) -> Result<Vec<usize>> {
    if spec != "auto" {
        return spec
            .split(',')
            .map(|c| Ok(c.trim().parse::<usize>()?))
            .collect();
    }
    let sys = |cpu: usize, file: &str| {
        std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/{file}"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    };
    // (max frequency, package, core) of each online CPU.
    let mut cores: Vec<(u64, u64, u64, usize)> = Vec::new();
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    for cpu in 0..n.max(256) {
        let Some(core) = sys(cpu, "topology/core_id") else {
            continue;
        };
        let package = sys(cpu, "topology/physical_package_id").unwrap_or(0);
        let freq = sys(cpu, "cpufreq/cpuinfo_max_freq").unwrap_or(0);
        if !cores.iter().any(|&(_, p, c, _)| p == package && c == core) {
            cores.push((freq, package, core, cpu));
        }
    }
    // Fastest first; among equals, in CPU order.
    cores.sort_by_key(|&(freq, _, _, cpu)| (std::cmp::Reverse(freq), cpu));
    let want = workers.unwrap_or(4);
    if cores.len() < want {
        bail!(
            "--cpus auto: {} physical cores, {want} asked for",
            cores.len()
        );
    }
    Ok(cores[..want].iter().map(|c| c.3).collect())
}

/// Governor and frequencies of `cpus`, to check they run at full speed.
fn describe_cpus(cpus: &[usize]) -> String {
    let read = |cpu: usize, file: &str| {
        std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/{file}"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "?".into())
    };
    cpus.iter()
        .map(|&c| {
            format!(
                "cpu{c} {} max {} of {} MHz",
                read(c, "scaling_governor"),
                read(c, "scaling_max_freq")
                    .parse::<u64>()
                    .map_or(0, |k| k / 1000),
                read(c, "cpuinfo_max_freq")
                    .parse::<u64>()
                    .map_or(0, |k| k / 1000),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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
fn run_bladerf(args: &Args, cpus: &[usize], registry: Registry) -> Result<()> {
    use std::sync::Arc;

    use bladerf_source::BladeRfSource;
    use bladerf_source::Radio;
    use plugin_api::BlockType;
    use plugin_api::Plugin;
    use plugin_api::add_kernel;

    // The receivers that take turns: --swap, else --halow and ZigBee.
    let flows = Path::new(env!("CARGO_MANIFEST_DIR")).join("flows");
    let names: Vec<String> = match &args.swap {
        Some(pair) => pair.split(',').map(|n| n.trim().to_string()).collect(),
        None => {
            let pair = [args.halow.clone(), "zigbee.toml".to_string()];
            match args.first.unwrap_or(Phy::Zigbee) {
                Phy::Halow => pair.to_vec(),
                Phy::Zigbee => vec![pair[1].clone(), pair[0].clone()],
            }
        }
    };
    let [a, b] = &names[..] else {
        bail!("--swap takes two descriptions: A,B");
    };
    let pair = [
        Description::from_file(flows.join(a))?,
        Description::from_file(flows.join(b))?,
    ];
    let phy = [phy_of(&pair[0])?, phy_of(&pair[1])?];
    let mut channels: Vec<u64> = pair
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
    channels.dedup();
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
        "rx_idx,phy,origin,event,len,frame,step,tag,ifs_us,ts_us,seq,rx_t_ms,swapped,swap_ms,\
         retune_ms,overflows"
    )?;
    let (duration, max_frames) = (Duration::from_secs_f64(args.duration), args.frames);
    let timeout = Duration::from_millis(args.rx_timeout_ms);
    let verbose = args.verbose;

    // Enter stops the run.
    static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if args.until_enter {
        std::thread::spawn(|| {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            STOP.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        println!("receiving; press Enter to stop");
    }
    let stopped = || STOP.load(std::sync::atomic::Ordering::Relaxed);

    let mut ctrl = controller(args, cpus, registry);
    ctrl.link("radio.samples", "rx.samples")?;
    let (swaps, retunes, counts) = ctrl.run(move |mut ctrl| async move {
        ctrl.spawn_async("radio", head).await?;
        let mut tap = ctrl.tap("rx.frames")?;
        ctrl.spawn_async("rx", pair[0].clone()).await?;
        println!(
            "listening with {} first; a frame changes to the other",
            pair[0].name.as_deref().unwrap_or("?")
        );

        let t0 = Instant::now();
        let (mut active, mut rx_idx) = (0usize, 0u64);
        let (mut swaps, mut retunes, mut counts) = (Vec::new(), Vec::new(), [0usize; 2]);
        let mut since_frame = Instant::now();
        let mut report = Instant::now();
        while !stopped()
            && t0.elapsed() < duration
            && (max_frames == 0 || rx_idx < max_frames as u64)
        {
            let wait = duration
                .saturating_sub(t0.elapsed())
                .min(Duration::from_millis(200));
            let frame = next_frame(&mut tap, wait).await;
            let rx_t = ms(t0.elapsed());
            let overflows = bladerf_source::OVERFLOWS.load(std::sync::atomic::Ordering::Relaxed);
            if report.elapsed() > Duration::from_secs(5) && !verbose {
                report = Instant::now();
                let mut recent = swaps.iter().rev().take(200).copied().collect::<Vec<f64>>();
                println!(
                    "[{:7.1} s] frames H {} Z {}, swaps {} (median {:.3} ms), overflows {}",
                    rx_t / 1e3,
                    counts[0],
                    counts[1],
                    swaps.len(),
                    median(&mut recent),
                    overflows
                );
            }
            // A frame of the PHY listened for changes receiver; so does
            // nothing for --rx-timeout-ms.
            let (got, timed_out) = match &frame {
                Some((origin, _)) => (phy_named(origin), false),
                None => (None, since_frame.elapsed() > timeout),
            };
            if frame.is_none() && !timed_out {
                continue;
            }
            let swap_now = timed_out || got == Some(phy[active]);
            let (mut swap, mut retune) = (f64::NAN, f64::NAN);
            if swap_now {
                since_frame = Instant::now();
                let t = Instant::now();
                let replacement = ctrl
                    .replace_async("rx", pair[1 - active].clone(), Hold::Discard)
                    .await?;
                swap = ms(t.elapsed());
                retune = ms(replacement.timings.controls);
                active = 1 - active;
                swaps.push(swap);
                retunes.push(retune);
            }

            let row = match &frame {
                None => format!("{rx_idx},?,,timeout,-1,-1,-1,-1,-1,-1,-1"),
                Some((origin, bytes)) => {
                    let p = got.unwrap_or(0);
                    counts[p] += 1;
                    let (stamp, seq) = if p == 1 {
                        (parse_stamp(bytes), None)
                    } else {
                        (None, parse_seq(bytes))
                    };
                    let s = |v: Option<String>| v.unwrap_or_else(|| "-1".into());
                    format!(
                        "{rx_idx},{},{origin},rx,{},{},{},{},{},{},{}",
                        if p == 0 { "H" } else { "Z" },
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
            writeln!(
                csv,
                "{row},{rx_t:.3},{},{swap:.3},{retune:.3},{overflows}",
                swap_now as u8
            )?;
            if verbose && let Some((origin, bytes)) = &frame {
                println!(
                    "[{rx_t:9.1} ms] {origin:14} {:4} B   swap {swap:.3} ms (retune {retune:.3})",
                    bytes.len()
                );
            }
            rx_idx += 1;
        }
        csv.flush().ok();
        stop_all(&mut ctrl).await?;
        anyhow::Ok((swaps, retunes, counts))
    })?;
    let (mut swaps, mut retunes) = (swaps, retunes);
    println!(
        "\n{} frames (H {}, Z {}), {} swaps: median {:.3} ms, retune median {:.3} ms; \
         quick-tune misses {}, radio overflows {} samples",
        counts[0] + counts[1],
        counts[0],
        counts[1],
        swaps.len(),
        median(&mut swaps),
        median(&mut retunes),
        radio.misses.load(std::sync::atomic::Ordering::Relaxed),
        bladerf_source::OVERFLOWS.load(std::sync::atomic::Ordering::Relaxed),
    );
    println!("wrote {}", csv_path.display());
    Ok(())
}

#[cfg(not(feature = "bladerf"))]
fn run_bladerf(_args: &Args, _cpus: &[usize], _registry: Registry) -> Result<()> {
    bail!("built without the `bladerf` feature; use --source replay")
}

// ── Replay ───────────────────────────────────────────────────────────────

/// The PHY of a receiver named `name`: `halow` or `zigbee`, or either
/// followed by `/` and anything (`halow/hard`).
fn phy_named(name: &str) -> Option<usize> {
    let phy = name.split('/').next().unwrap_or_default();
    PHYS.iter().position(|p| *p == phy)
}

/// The PHY a receiver description posts frames of, by its name.
fn phy_of(desc: &Description) -> Result<usize> {
    let name = desc.name.as_deref().unwrap_or_default();
    phy_named(name)
        .ok_or_else(|| anyhow::anyhow!("a receiver named {name:?}: expected halow or zigbee"))
}

/// Stop every flowgraph; those of a swappable block stop with theirs.
async fn stop_all(ctrl: &mut Controller) -> Result<()> {
    let names: Vec<String> = ctrl.names().map(str::to_string).collect();
    for name in names {
        if ctrl.names().any(|n| n == name) {
            ctrl.stop_async(&name).await?;
        }
    }
    Ok(())
}

/// `name:count` of each description that posted frames.
fn posted_by(origins: &BTreeMap<String, usize>) -> String {
    origins
        .iter()
        .map(|(o, n)| format!("{o}:{n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What one IFS of the replay gave.
struct StepResult {
    /// Frames from a receiver that was listening when they were sent, by
    /// PHY.
    received: [usize; 2],
    /// Frames a radio could not have given: the receiver was listening for
    /// none of the transmission it decoded. Not counted as received.
    impossible: usize,
    /// Swap times, by the receiver swapped to (which the swap builds).
    swaps: [Vec<f64>; 2],
    retunes: Vec<f64>,
    /// Transmissions decoded more than once (counted once).
    duplicates: usize,
    /// Lost transmissions that started before the receiver was listening
    /// for their PHY: the swap was late.
    late: usize,
    /// Lost transmissions the receiver listened to from their start.
    missed: usize,
    /// Items queued on the link when each swap began.
    queued: Vec<f64>,
    /// Frames posted, by the description that posted them.
    origins: BTreeMap<String, usize>,
    /// Samples the replay could not deliver in time, and samples the link
    /// to the receiver dropped because it was full: what a radio would have
    /// lost to a receiver that fell behind.
    source_dropped: usize,
    link_dropped: u64,
}

/// Replay step `step` to `pair`, the receivers taking turns from the first.
async fn replay_step(
    ctrl: &mut Controller,
    step: usize,
    retune_us: u64,
    pair: &[Description; 2],
    sent: &[replay::Sent],
    no_swap: bool,
) -> Result<StepResult> {
    let phy = [phy_of(&pair[0])?, phy_of(&pair[1])?];
    *replay::STARTED.lock().unwrap() = None;
    replay::LATE_CHUNKS.lock().unwrap().clear();
    replay::SOURCE_DROPPED.store(0, std::sync::atomic::Ordering::Relaxed);
    let link_dropped_before = ctrl.link_stats("rx.samples").map_or(0, |s| s.dropped);
    replay::load(step);
    // The radio first: a receiver that starts on an ended stream ends.
    ctrl.spawn_async(
        "radio",
        Description::from_toml(&replay::head(step, retune_us))?,
    )
    .await?;
    let mut tap = ctrl.tap("rx.frames")?;
    ctrl.spawn_async("rx", pair[0].clone()).await?;

    let mut received = [0usize; 2];
    let mut impossible = 0;
    let mut duplicates = 0;
    let mut decoded = vec![false; sent.len()];
    // REPLAY_SWAPS=N: what the first N swaps did, and whether what they
    // replaced terminated.
    let show_swaps: usize = std::env::var("REPLAY_SWAPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut retired = Vec::new();
    let mut seen_ids = Vec::new();
    // Frames posted, by the description that posted them.
    let mut origins: BTreeMap<String, usize> = BTreeMap::new();
    let mut latency = Vec::new();
    let mut queued = Vec::new();
    // Each swap: the decoded frame's end, when it was posted, when the swap
    // was done (replay clock, s), and the items queued when it began.
    let mut log: Vec<(f64, f64, f64, usize)> = Vec::new();
    let (mut swaps, mut retunes) = ([Vec::new(), Vec::new()], Vec::new());
    // Which PHY the receiver listened for, from a given time on.
    let mut listening: Vec<(f64, usize)> = vec![(0.0, phy[0])];
    let mut active = 0;
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
        let got = phy_named(&origin).unwrap();
        *origins.entry(origin.clone()).or_insert(0) += 1;

        // The transmission it decodes: the last of its PHY that ended before
        // it arrived (every copy is the same recording).
        let at = replay::now();
        let index = sent.iter().rposition(|s| s.phy == got && s.end <= at);
        let frame = index.map(|k| &sent[k]);
        if let Some(f) = frame {
            latency.push(at - f.end);
        }
        let listened = frame.is_some_and(|f| {
            listening.iter().enumerate().any(|(k, (from, p))| {
                let until = listening.get(k + 1).map_or(f64::INFINITY, |(t, _)| *t);
                *p == got && *from < f.end && until > f.start
            })
        });
        match index {
            Some(k) if listened && decoded[k] => duplicates += 1,
            Some(k) if listened => {
                decoded[k] = true;
                received[got] += 1;
            }
            _ => impossible += 1,
        }
        if got != phy[active] || no_swap {
            continue;
        }
        let next = 1 - active;
        let waiting = ctrl.link_stats("rx.samples").map_or(0, |s| s.queued);
        queued.push(waiting as f64);
        let t = Instant::now();
        let replacement = ctrl
            .replace_async("rx", pair[next].clone(), Hold::Discard)
            .await?;
        swaps[next].push(ms(t.elapsed()));
        retunes.push(ms(replacement.timings.controls));
        if show_swaps > 0 {
            let handle = ctrl.handle("rx").expect("running");
            let blocks = handle.describe().map_or(0, |d| d.blocks.len());
            let fresh = !seen_ids.contains(&handle.id());
            if fresh {
                seen_ids.push(handle.id());
            }
            if retired.len() < show_swaps {
                let t = &replacement.timings;
                eprintln!(
                    "swap {:3}: {} -> {}: flowgraph {:?} ({}), {blocks} blocks; build {:.3} ms, \
                     start {:.3} ms, switch {:.3} ms, total {:.3} ms",
                    retired.len(),
                    replacement.old.name(),
                    pair[next].name.as_deref().unwrap_or("?"),
                    handle.id(),
                    if fresh { "new" } else { "SEEN BEFORE" },
                    ms(t.build),
                    ms(t.start),
                    ms(t.switch),
                    ms(t.total),
                );
            }
            retired.push(replacement.old);
        }
        active = next;
        let done = replay::now();
        listening.push((done, phy[next]));
        log.push((frame.map_or(f64::NAN, |f| f.end), at, done, waiting));
    }
    let link_dropped_after = ctrl.link_stats("rx.samples").map_or(0, |s| s.dropped);
    stop_all(ctrl).await?;
    replay::unload(step);
    if show_swaps > 0 {
        let n = retired.len();
        let mut ended = 0;
        for old in retired {
            if old.wait_async().await.is_ok() {
                ended += 1;
            }
        }
        eprintln!(
            "step {step}: {n} swaps, {} distinct new flowgraphs, {ended} of the {n} replaced \
             ones terminated",
            seen_ids.len()
        );
    }
    if std::env::var_os("REPLAY_LOSSES").is_some() {
        let slow = latency.iter().filter(|l| **l > 3e-4).count();
        let worst = latency.iter().copied().fold(0.0, f64::max);
        eprintln!(
            "step {step}: {} frames posted, {slow} more than 0.3 ms after their end, worst {:.3} ms, \
             median {:.3} ms",
            latency.len(),
            worst * 1e3,
            median(&mut latency.clone()) * 1e3
        );
        let late = replay::LATE_CHUNKS.lock().unwrap();
        let worst = late.iter().map(|(_, l)| *l).fold(0.0, f64::max);
        eprintln!(
            "step {step}: {} replay chunks more than 0.2 ms late, worst {:.3} ms",
            late.len(),
            worst * 1e3
        );
    }
    // Why the others were lost: whether the receiver was listening for
    // their PHY from before they started.
    let (mut late, mut missed) = (0, 0);
    for (f, _) in sent.iter().zip(&decoded).filter(|(_, d)| !**d) {
        let before = listening.iter().rfind(|(from, _)| *from <= f.start);
        let changed = listening
            .iter()
            .any(|(from, _)| *from > f.start && *from < f.end);
        if before.is_some_and(|(_, p)| *p == f.phy) && !changed {
            missed += 1;
        } else {
            late += 1;
            if std::env::var_os("REPLAY_LOSSES").is_some() {
                // The swap before it, or the one during it.
                if let Some((end, posted, done, waiting)) =
                    log.iter().rfind(|(_, _, done, _)| *done < f.end)
                {
                    // Replay chunks that came late around the previous frame.
                    let late_chunks: Vec<String> = replay::LATE_CHUNKS
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(t, _)| *t > end - 2e-3 && *t < *posted)
                        .map(|(t, l)| format!("{:.2}@{:.3}", l * 1e3, (t - end) * 1e3))
                        .collect();
                    eprintln!(
                        "lost at {:8.3} ms: previous frame ended {:7.3} ms before it, posted \
                         {:6.3} ms after its end with {waiting} items queued, swap done {:6.3} ms \
                         after the lost frame began; late replay chunks (ms late @ ms after \
                         that end): {late_chunks:?}",
                        f.start * 1e3,
                        (f.start - end) * 1e3,
                        (posted - end) * 1e3,
                        (done - f.start) * 1e3,
                    );
                }
            }
        }
    }
    Ok(StepResult {
        received,
        impossible,
        swaps,
        retunes,
        duplicates,
        late,
        missed,
        queued,
        origins,
        source_dropped: replay::SOURCE_DROPPED.load(std::sync::atomic::Ordering::Relaxed),
        link_dropped: link_dropped_after - link_dropped_before,
    })
}

fn run_replay(args: &Args, cpus: &[usize], mut registry: Registry) -> Result<()> {
    registry.register(replay::blocks())?;
    replay::SOURCE_BUFFER.store(args.source_buffer, std::sync::atomic::Ordering::Relaxed);
    replay::CHUNK.store(args.chunk.max(1), std::sync::atomic::Ordering::Relaxed);
    let testdata = workspace().join("blocks/testdata");
    let mut recordings = [
        replay::read_cf32(&testdata.join("halow_frame.cf32"))?,
        replay::read_cf32(&testdata.join("zigbee_frame.cf32"))?,
    ];
    // Silence each replayed recording keeps before and after its frame, in
    // ms: none once cut to the frame.
    let mut silence = [[0.0f64; 2]; 2];
    for ((recording, name), margin) in recordings.iter_mut().zip(PHYS).zip(&mut silence) {
        let us = |n: usize| n as f64 / replay::RATE * 1e6;
        let burst = replay::burst(recording);
        if args.no_trim {
            *margin = [us(burst.start) / 1e3, us(recording.len() - burst.end) / 1e3];
        }
        println!(
            "{name}: recording {:.0} µs, frame {:.1} µs, silence {:.1} µs before and {:.1} after{}",
            us(recording.len()),
            us(burst.len()),
            us(burst.start),
            us(recording.len() - burst.end),
            if args.no_trim {
                ""
            } else {
                ": cut to the frame"
            },
        );
        if !args.no_trim {
            *recording = recording[burst].to_vec();
        }
    }
    if args.first.is_some() {
        bail!("--first does not apply to the replay: the first receiver of the pair starts");
    }
    let pairs: Vec<[String; 2]> = match &args.swap {
        Some(pair) => {
            let names: Vec<&str> = pair.split(',').map(str::trim).collect();
            let [a, b] = names[..] else {
                bail!("--swap takes two descriptions: A,B");
            };
            vec![[a.to_string(), b.to_string()]]
        }
        None => args
            .halow
            .split(',')
            .map(|h| [h.trim().to_string(), "zigbee.toml".to_string()])
            .collect(),
    };
    let frames = args.frames_per_step.max(2);
    let ifs = parse_sweep(&args.ifs)?;
    let flows = Path::new(env!("CARGO_MANIFEST_DIR")).join("flows");
    let label = |ms: Option<f64>| ms.map_or("the same".to_string(), |v| format!("{v} ms"));
    println!(
        "{frames} frames per IFS, IFS after the second receiver's frames {}, {}, chunks of {} \
         samples",
        label(args.ifs_after_second),
        if args.retune_us == 0 {
            "no retuning".to_string()
        } else {
            format!("retune {} µs", args.retune_us)
        },
        args.chunk,
    );

    let mut table = String::from(
        "first,second,ifs_ms,ifs_after_second_ms,ifs_true_ms,ifs_true_after_first_ms,\
         ifs_true_after_second_ms,sent_h,rx_h,sent_z,rx_z,per_h,per_z,per,\
         impossible,swaps,swap_median_ms,swap_to_first_median_ms,swap_to_second_median_ms,\
         swap_max_ms,retune_median_ms,duplicates,lost_late,lost_listening,queued_median,\
         queued_max,posted_by,source_dropped,link_dropped\n",
    );
    for [first, second] in pairs {
        let pair = [
            Description::from_file(flows.join(&first))?,
            Description::from_file(flows.join(&second))?,
        ];
        let phy = [phy_of(&pair[0])?, phy_of(&pair[1])?];
        let steps = replay::prepare(
            [&recordings[phy[0]], &recordings[phy[1]]],
            [phy[0], phy[1]],
            frames,
            ifs.clone(),
            args.ifs_after_second,
        );
        let mut sent = [0usize; 2];
        for s in &steps.sent[0] {
            sent[s.phy] += 1;
        }
        println!("\n{first} → {second} → ...");
        println!(
            "  IFS ms    PER H    PER Z    PER   swap median (→{first:.5} / →{second:.5}) / max ms   \
             lost: late listening   queued at swap: median max"
        );
        let mut ctrl = controller(args, cpus, registry.clone());
        ctrl.link("radio.samples", "rx.samples")?;
        let retune_us = args.retune_us;
        let no_swap = args.no_swap;
        if no_swap && phy[0] != phy[1] {
            bail!("--no-swap needs the pair to be of one PHY");
        }
        let per_step = steps.sent.clone();
        let results = ctrl.run(move |mut ctrl| async move {
            let mut results = Vec::new();
            for (step, sent) in per_step.iter().enumerate() {
                results.push(replay_step(&mut ctrl, step, retune_us, &pair, sent, no_swap).await?);
            }
            anyhow::Ok(results)
        })?;

        for ((ifs, after), mut r) in steps
            .ifs_ms
            .iter()
            .zip(&steps.ifs_after_second_ms)
            .zip(results)
        {
            let rate = |k: usize| {
                if sent[k] == 0 {
                    f64::NAN
                } else {
                    1.0 - r.received[k] as f64 / sent[k] as f64
                }
            };
            let (per_h, per_z) = (rate(0), rate(1));
            // The IFS from frame end to next frame start: the gap, and the
            // silence the recordings keep on either side (none when cut).
            let true_first = ifs + silence[phy[0]][1] + silence[phy[1]][0];
            let true_second = after + silence[phy[1]][1] + silence[phy[0]][0];
            let true_mean = (true_first + true_second) / 2.0;
            let per = 1.0 - (r.received[0] + r.received[1]) as f64 / (sent[0] + sent[1]) as f64;
            let mut all: Vec<f64> = r.swaps.concat();
            let n = all.len();
            let swap = median(&mut all);
            let max = all.last().copied().unwrap_or(f64::NAN);
            let to_first = median(&mut r.swaps[0]);
            let to_second = median(&mut r.swaps[1]);
            let retune = median(&mut r.retunes);
            let queued_max = r.queued.iter().copied().fold(0.0, f64::max);
            let queued_median = median(&mut r.queued);
            let pct = |v: f64| {
                if v.is_nan() {
                    "     -".to_string()
                } else {
                    format!("{:5.1}%", 100.0 * v)
                }
            };
            println!(
                "  {ifs:6.2}   {}   {}   {}   {swap:6.3} ({to_first:.3} / {to_second:.3}) / \
                 {max:6.3}   {:4} {:4}   {queued_median:6.0} {queued_max:6.0}",
                pct(per_h),
                pct(per_z),
                pct(per),
                r.late,
                r.missed,
            );
            writeln!(
                table,
                "{first},{second},{ifs},{after},{true_mean:.4},{true_first:.4},{true_second:.4},\
                 {},{},{},{},{per_h:.4},{per_z:.4},{per:.4},{},{n},\
                 {swap:.4},{to_first:.4},{to_second:.4},{max:.4},{retune:.4},{},{},{},\
                 {queued_median},{queued_max},{},{},{}",
                sent[0],
                r.received[0],
                sent[1],
                r.received[1],
                r.impossible,
                r.duplicates,
                r.late,
                r.missed,
                posted_by(&r.origins),
                r.source_dropped,
                r.link_dropped,
            )?;
            if r.source_dropped + r.link_dropped as usize > 0 {
                println!(
                    "           dropped: {} samples by the replay, {} by the link",
                    r.source_dropped, r.link_dropped
                );
            }
            if r.origins.len() > 1 {
                println!("           posted by {}", posted_by(&r.origins));
            }
        }
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
    // Plugins build with every CPU; the runs then use those asked for.
    let registry = registry(&args)?;
    let (cpus, source_cpus) = match &args.cpus {
        // The runtime's threads on the fastest cores, the source's thread
        // on the next one, if there is one more.
        Some(spec) if spec == "auto" => {
            let want = args.workers.unwrap_or(4);
            match parse_cpus(spec, Some(want + 1)) {
                Ok(mut all) => {
                    let source = all.pop().into_iter().collect();
                    (all, source)
                }
                Err(_) => (parse_cpus(spec, Some(want))?, Vec::new()),
            }
        }
        Some(spec) => {
            let cpus = parse_cpus(spec, args.workers)?;
            let n = std::thread::available_parallelism().map_or(1, |n| n.get());
            let others = (0..n).filter(|c| !cpus.contains(c)).collect();
            (cpus, others)
        }
        None => (Vec::new(), Vec::new()),
    };
    if !cpus.is_empty() {
        // The timer thread (async-io's) keeps every CPU.
        futuresdr::runtime::block_on(Timer::after(Duration::from_micros(1)));
        replay::set_thread_cpus(&cpus)?;
        println!("runtime threads on {}", describe_cpus(&cpus));
        // Without CPUs of its own, the source shares the runtime's.
        let source = if source_cpus.is_empty() {
            cpus.clone()
        } else {
            source_cpus
        };
        println!("source thread on {}", describe_cpus(&source));
        if args.keep_awake {
            let mut all = cpus.clone();
            all.extend(source.iter().filter(|c| !cpus.contains(c)));
            replay::keep_awake(&all)?;
            println!("keeping CPUs {all:?} awake");
        }
        *replay::SOURCE_CPUS.lock().unwrap() = source;
    }
    if let Some(priority) = args.rt_priority {
        // Before the runtime starts its threads, which inherit it.
        replay::set_thread_rt_priority(priority)?;
        replay::RT_PRIORITY.store(priority, std::sync::atomic::Ordering::Relaxed);
        println!("real-time priority {priority} (SCHED_FIFO)");
    }
    match args.source {
        Source::Bladerf => run_bladerf(&args, &cpus, registry),
        Source::Replay => run_replay(&args, &cpus, registry),
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
