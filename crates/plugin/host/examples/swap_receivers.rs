//! Replace a running receiver over and over while its source keeps streaming.
//!
//! ```text
//! source ──samples──▶ receiver    (flows/rx_a.toml ⇄ flows/rx_b.toml)
//! ```
//!
//! All flowgraphs are TOML descriptions in `examples/flows`; their block
//! types come from the `basic` plugin (`crates/plugin/blocks/basic`),
//! compiled here against this program's own copy of the shared library —
//! or loaded from `--plugins <dir>`.
//!
//! ```text
//! cd crates/plugin
//! cargo run --example swap_receivers -- --hold keep --every-ms 30
//! ```
//!
//! Receiver B negates the samples, so the output shows which receiver
//! handled each one. With `--hold keep` every sample reaches exactly one
//! receiver, which the program checks.
//!
//! The source's `Throttle` releases samples in bursts every 100 ms: a
//! replacement period that divides 100 ms keeps handing the bursts to the
//! same receiver.

use std::path::Path;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use futuresdr::blocks::VectorSink;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Finished;
use plugin_host::Hold;
use plugin_host::Registry;
use plugin_host::ReplaceTimings;
use plugin_sdk::Sdk;

fn samples(done: &Finished) -> Result<Vec<f32>> {
    Ok(done.block::<VectorSink<f32>>("snk")?.items().clone())
}

fn stats(label: &str, values: impl Iterator<Item = Duration>) {
    let mut ms: Vec<f64> = values.map(|d| d.as_secs_f64() * 1e3).collect();
    ms.sort_by(f64::total_cmp);
    if ms.is_empty() {
        return;
    }
    println!(
        "  {label:<7} median {:7.3} ms   max {:7.3} ms",
        ms[ms.len() / 2],
        ms[ms.len() - 1]
    );
}

fn main() -> Result<()> {
    let mut hold = Hold::Keep;
    let mut every = Duration::from_millis(30);
    let mut plugins: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--hold", Some(v)) if v == "keep" => hold = Hold::Keep,
            ("--hold", Some(v)) if v == "discard" => hold = Hold::Discard,
            ("--every-ms", Some(v)) => every = Duration::from_millis(v.parse()?),
            ("--plugins", Some(v)) => plugins = Some(v.into()),
            _ => {
                bail!("usage: swap_receivers [--hold keep|discard] [--every-ms N] [--plugins DIR]")
            }
        }
    }

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let flows = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/flows");
    let mut registry = Registry::new();
    match plugins {
        Some(dir) => {
            registry.load_dir(&dir)?;
        }
        None => {
            let library = Sdk::of_this_process()?.build_plugin(
                &workspace.join("blocks/basic/Cargo.toml"),
                &workspace.join("target/plugins"),
            )?;
            registry.load(&library)?;
        }
    }
    println!("{} block types available", registry.type_names().count());

    let receivers = [
        Description::from_file(flows.join("rx_a.toml"))?,
        Description::from_file(flows.join("rx_b.toml"))?,
    ];
    let mut ctrl = Controller::new(registry);
    ctrl.link("source.samples", "receiver.samples")?;
    ctrl.spawn("receiver", receivers[0].clone())?;
    ctrl.spawn("source", Description::from_file(flows.join("source.toml"))?)?;

    let mut retired = Vec::new();
    let mut timings: Vec<ReplaceTimings> = Vec::new();
    let mut next = 1;
    while !ctrl.link_stats("source.samples").unwrap().closed {
        sleep(every);
        let replaced = ctrl.replace("receiver", receivers[next].clone(), hold)?;
        timings.push(replaced.timings);
        retired.push(replaced.old);
        next = 1 - next;
    }
    ctrl.wait("source")?;

    let mut segments = Vec::new();
    for old in retired {
        segments.push(samples(&old.wait()?)?);
    }
    segments.push(samples(&ctrl.wait("receiver")?)?);

    println!("{} replacements ({hold:?}), every {every:?}", timings.len());
    stats("build", timings.iter().map(|t| t.build));
    stats("start", timings.iter().map(|t| t.start));
    stats("switch", timings.iter().map(|t| t.switch));
    stats("total", timings.iter().map(|t| t.total));

    let all: Vec<f32> = segments.iter().flatten().copied().collect();
    let by_b = all.iter().filter(|v| v.is_sign_negative()).count();
    println!(
        "{} samples: {} by receiver A, {by_b} by receiver B",
        all.len(),
        all.len() - by_b
    );
    let indices: Vec<u64> = all.iter().map(|v| v.abs() as u64).collect();
    let in_order = indices.windows(2).all(|w| w[0] < w[1]);
    match hold {
        Hold::Keep if indices.iter().copied().eq(0..400_000) => {
            println!("every sample arrived once, in order")
        }
        Hold::Discard if in_order => println!(
            "{} samples dropped during replacements, the rest in order",
            400_000 - indices.len()
        ),
        _ => bail!("samples lost, repeated or out of order"),
    }
    Ok(())
}
