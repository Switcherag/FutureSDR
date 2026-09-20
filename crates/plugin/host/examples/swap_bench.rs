//! Replacement latency, measured the way the `dyn` branch's `cross_fg_bench`
//! measures its swaps.
//!
//! ```text
//! source (NullSource<u8> > Throttle<u8>) ──bytes──▶ receiver
//!                                         (flows/bench_a.toml ⇄ flows/bench_b.toml)
//! ```
//!
//! Each replacement is timed from the description file to the new receiver
//! owning the stream, so reading and parsing the TOML is included, as in
//! `dyn`. The old receiver finishes in the background.
//!
//! By default the replacements are made from a task of the runtime
//! (`--driver task`, as `dyn` does); `--driver thread` makes them from the
//! main thread, which then waits for the runtime.
//!
//! With `--mode standby`, the next receiver is prepared right after each
//! replacement, and the replacement is only the commit: the time it takes
//! is how long the stream waits. Preparing is reported apart.
//!
//! ```text
//! cd crates/plugin
//! cargo run --release --example swap_bench -- --iterations 50 --rate 1
//! ```
//!
//! Options: `--iterations N` (replacements per receiver), `--rate R` (source
//! bytes per second), `--hold keep|discard`, `--settle-ms MS` (pause between
//! replacements), `--mode on-demand|standby`, `--driver task|thread`,
//! `--workers N` (runtime threads, one per core by default), `--pool-limit
//! BYTES` (buffer rings kept between flowgraphs; 0 maps every buffer anew),
//! `--plugins DIR`, `--csv FILE`.

use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use anyhow::bail;
use futuresdr::runtime::Runtime;
use futuresdr::runtime::Timer;
use futuresdr::runtime::block_on;
use futuresdr::runtime::scheduler::SmolScheduler;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Hold;
use plugin_host::Registry;
use plugin_sdk::Sdk;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn summary(label: &str, values: &[f64]) {
    if values.is_empty() {
        return;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len() as f64;
    let mean = sorted.iter().sum::<f64>() / n;
    let sd = (sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
    let p99 = sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize];
    println!(
        "  {label:<8} mean {mean:7.3}  sd {sd:6.3}  min {:7.3}  median {:7.3}  p99 {p99:7.3}  max {:7.3}  ms",
        sorted[0],
        sorted[sorted.len() / 2],
        sorted[sorted.len() - 1],
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    Task,
    Thread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Read, build, start and switch to the next receiver at once.
    OnDemand,
    /// Switch to a receiver prepared in advance.
    Standby,
}

#[derive(Default)]
struct Samples {
    rows: String,
    parse: Vec<f64>,
    build: Vec<f64>,
    start: Vec<f64>,
    switch: Vec<f64>,
    /// On demand: the replacement; on standby: preparing the next one.
    prepare: Vec<f64>,
    /// How long the replacement kept the stream waiting.
    total: Vec<f64>,
}

impl Samples {
    fn new() -> Self {
        Self {
            rows: String::from(
                "iteration,swap,target,parse_ms,build_ms,start_ms,switch_ms,prepare_ms,total_ms\n",
            ),
            ..Self::default()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        iteration: usize,
        swap: usize,
        target: &Path,
        parse: Duration,
        build: Duration,
        start: Duration,
        switch: Duration,
        prepare: Duration,
        total: Duration,
    ) -> Result<()> {
        let values = [parse, build, start, switch, prepare, total].map(ms);
        for (list, v) in [
            &mut self.parse,
            &mut self.build,
            &mut self.start,
            &mut self.switch,
            &mut self.prepare,
            &mut self.total,
        ]
        .into_iter()
        .zip(values)
        {
            list.push(v);
        }
        write!(
            self.rows,
            "{iteration},{swap},{}",
            target.file_name().unwrap().to_string_lossy()
        )?;
        for v in values {
            write!(self.rows, ",{v:.4}")?;
        }
        writeln!(self.rows)?;
        Ok(())
    }
}

async fn on_demand(
    ctrl: &mut Controller,
    targets: &[PathBuf],
    iterations: usize,
    hold: Hold,
    settle: Duration,
) -> Result<Samples> {
    let mut s = Samples::new();
    for iteration in 0..iterations {
        for (swap, target) in targets.iter().enumerate() {
            let t = Instant::now();
            let desc = Description::from_file(target)?;
            let parsed = t.elapsed();
            let replaced = ctrl.replace_async("receiver", desc, hold).await?;
            let elapsed = t.elapsed();
            drop(replaced.old);

            let r = replaced.timings;
            s.push(
                iteration, swap, target, parsed, r.build, r.start, r.switch, r.total, elapsed,
            )?;
            Timer::after(settle).await;
        }
    }
    Ok(s)
}

async fn standby(
    ctrl: &mut Controller,
    targets: &[PathBuf],
    iterations: usize,
    hold: Hold,
    settle: Duration,
) -> Result<Samples> {
    let mut s = Samples::new();
    let mut next = ctrl
        .prepare_async("receiver", Description::from_file(&targets[0])?)
        .await?;
    for iteration in 0..iterations {
        for (swap, target) in targets.iter().enumerate() {
            Timer::after(settle).await;
            let t = Instant::now();
            let old = ctrl.commit_async(next, hold).await?;
            let switched = t.elapsed();
            drop(old);

            let t = Instant::now();
            let following = &targets[(swap + 1) % targets.len()];
            let desc = Description::from_file(following)?;
            let parsed = t.elapsed();
            next = ctrl.prepare_async("receiver", desc).await?;
            let prepared = t.elapsed();
            s.push(
                iteration,
                swap,
                target,
                parsed,
                next.build_time(),
                next.start_time(),
                switched,
                prepared,
                switched,
            )?;
        }
    }
    Ok(s)
}

async fn replacements(
    ctrl: &mut Controller,
    mode: Mode,
    targets: &[PathBuf],
    iterations: usize,
    hold: Hold,
    settle: Duration,
) -> Result<Samples> {
    match mode {
        Mode::OnDemand => on_demand(ctrl, targets, iterations, hold, settle).await,
        Mode::Standby => standby(ctrl, targets, iterations, hold, settle).await,
    }
}

fn main() -> Result<()> {
    let mut iterations = 50;
    let mut rate = 1.0;
    let mut hold = Hold::Discard;
    let mut driver = Driver::Task;
    let mut mode = Mode::OnDemand;
    let mut workers: Option<usize> = None;
    let mut poll: Option<Duration> = None;
    let mut settle = Duration::from_millis(200);
    let mut plugins: Option<PathBuf> = None;
    let mut csv: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--iterations", Some(v)) => iterations = v.parse()?,
            ("--rate", Some(v)) => rate = v.parse()?,
            ("--hold", Some(v)) if v == "keep" => hold = Hold::Keep,
            ("--hold", Some(v)) if v == "discard" => hold = Hold::Discard,
            ("--settle-ms", Some(v)) => settle = Duration::from_millis(v.parse()?),
            ("--workers", Some(v)) => workers = Some(v.parse()?),
            ("--poll-us", Some(v)) => poll = Some(Duration::from_micros(v.parse()?)),
            ("--mode", Some(v)) if v == "on-demand" => mode = Mode::OnDemand,
            ("--mode", Some(v)) if v == "standby" => mode = Mode::Standby,
            ("--driver", Some(v)) if v == "task" => driver = Driver::Task,
            ("--driver", Some(v)) if v == "thread" => driver = Driver::Thread,
            ("--plugins", Some(v)) => plugins = Some(v.into()),
            ("--csv", Some(v)) => csv = Some(v.into()),
            ("--pool-limit", Some(v)) => {
                futuresdr_plugin_rt::buffer::set_pool_limit(v.parse()?);
            }
            _ => bail!(
                "usage: swap_bench [--iterations N] [--rate R] [--hold keep|discard] \
                 [--settle-ms MS] [--mode on-demand|standby] [--driver task|thread] [--workers N] \
                 [--pool-limit BYTES] [--plugins DIR] [--csv FILE]"
            ),
        }
    }

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let flows = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/flows");
    let mut registry = Registry::new();
    let libraries = match plugins {
        Some(dir) => {
            let t = Instant::now();
            let loaded = registry.load_dir(&dir)?;
            println!(
                "loaded {} plugin(s) in {:.3} ms",
                loaded.len(),
                ms(t.elapsed())
            );
            loaded
        }
        None => {
            let library = Sdk::of_this_process()?.build_plugin(
                &workspace.join("blocks/basic/Cargo.toml"),
                &workspace.join("target/plugins"),
            )?;
            let t = Instant::now();
            registry.load(&library)?;
            println!("loaded {} in {:.3} ms", library.display(), ms(t.elapsed()));
            vec![library]
        }
    };
    for library in &libraries {
        println!(
            "  {} ({} bytes)",
            library.display(),
            std::fs::metadata(library)?.len()
        );
    }
    println!("{} block types available", registry.type_names().count());

    let source = Description::from_toml(&format!(
        r#"
        name = "source"
        connections = "src > pace"

        [blocks.src]
        type = "NullSource<u8>"

        [blocks.pace]
        type = "Throttle<u8>"
        rate = {rate:?}

        [outputs]
        bytes = "pace.output"
        "#
    ))?;
    let targets = [flows.join("bench_a.toml"), flows.join("bench_b.toml")];

    let mut ctrl = match workers {
        Some(n) => Controller::with_runtime(
            Runtime::with_scheduler(SmolScheduler::with_config(n, false)),
            registry,
        ),
        None => Controller::new(registry),
    };
    if let Some(period) = poll {
        // What dyn's idle bridges do: wake up every `period`.
        ctrl.runtime().spawn_background(async move {
            loop {
                Timer::after(period).await;
            }
        });
    }
    ctrl.link("source.bytes", "receiver.bytes")?;
    ctrl.spawn("source", source)?;
    ctrl.spawn("receiver", Description::from_file(&targets[1])?)?;
    sleep(Duration::from_secs(1));

    println!(
        "{} replacements ({iterations} x {} receivers), rate {rate} bytes/s, {hold:?}, \
         settle {settle:?}, {mode:?} from a {driver:?}",
        iterations * targets.len(),
        targets.len()
    );
    let (mut ctrl, s) = match driver {
        Driver::Task => ctrl.run(move |mut ctrl| async move {
            let s = replacements(&mut ctrl, mode, &targets, iterations, hold, settle).await;
            (ctrl, s)
        }),
        Driver::Thread => {
            let s = block_on(replacements(
                &mut ctrl, mode, &targets, iterations, hold, settle,
            ));
            (ctrl, s)
        }
    };
    let s = s?;

    summary("parse", &s.parse);
    summary("build", &s.build);
    summary("start", &s.start);
    summary("switch", &s.switch);
    match mode {
        Mode::OnDemand => summary("replace", &s.prepare),
        Mode::Standby => summary("prepare", &s.prepare),
    }
    summary("total", &s.total);
    let stats = ctrl.link_stats("source.bytes").unwrap();
    println!("link: {} queued, {} dropped", stats.queued, stats.dropped);

    ctrl.stop("source")?;
    ctrl.wait("receiver")?;
    if let Some(path) = csv {
        std::fs::write(&path, s.rows)?;
        println!("timings written to {}", path.display());
    }
    Ok(())
}
