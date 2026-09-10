// softswap_latency — what a PHY swap costs in software, with no radio in it.
//
// The dual-PHY swaps in this directory all measure the same thing twice over:
// the cost of tearing one flowgraph down and standing another up, *plus* the
// cost of moving the LO across bands. The second term dominates — 27 ms with
// FPGA tuning, 112 ms without — varies with the hardware, and drowns the
// first. This binary removes it.
//
//   head:  flows/null_head.toml   NullSource<Complex32>, no sdr_block
//   tail:  flows/null_tail.toml   accepts the tail bridge and discards it
//   swaps: flows/zigbee_rxA.toml  <->  flows/halow_rxA.toml
//
// The two swappable flows are the real ones, unmodified, `[radio]` sections
// and all — but the null head declares no `sdr_block`, so `apply_radio_demand`
// has nothing to dispatch to and step 1 of the swap returns immediately. What
// is left is the software: gating the head off, parking selectors, loading
// plugins, building and starting the incoming flowgraph, reconnecting, handing
// the outgoing one to a background thread, unparking.
//
// Nothing is ever received, and that is deliberate rather than a limitation.
// The frame-driven siblings swap when a frame arrives, so their swap rate is
// the transmitter's frame rate and their sample is whatever the air gave them.
// Here swaps are driven on a timer, so the count is chosen, evenly spaced, and
// identical between runs — which is what makes the distribution comparable.
//
// Output: softswap_latency.csv, one row per swap, one column per swap step.
// `plot/softswap_stats.py` turns it into per-step statistics.
//
// Run from this directory so the relative TOML paths resolve:
//   cd examples/real_device_swap && ../../target/release/softswap_latency

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use futuresdr::async_io::Timer;
use plugin_host::{FlowgraphController, SwapTimings, default_plugin_dir};

const FLOW_Z: &str = "flows/zigbee_rxA.toml";
/// Default 802.11ah flow. `--flow-h` overrides it, because which receiver is
/// being stood up dominates the swap cost — halow_rxA is 9 blocks and 8
/// connections, halowv6A is 14 and 17 — so a figure quoted without saying
/// which flow it built is not comparable with anything.
const FLOW_H: &str = "flows/halow_rxA.toml";
const HEAD_FLOW: &str = "flows/null_head.toml";
const TAIL_FLOW: &str = "flows/null_tail.toml";

#[derive(Parser)]
#[command(about = "Swap-latency benchmark with a null head — no radio in the loop")]
struct Args {
    /// Swaps to measure, alternating Zigbee <-> HaLow.
    #[arg(long, default_value_t = 200)]
    swaps: usize,

    /// Swaps to run and discard first. The first swap of a process pays for
    /// loading each PHY's plugins off disk, which is a one-time cost and not
    /// what this measures; two warmups put both flows' plugins in the registry.
    #[arg(long, default_value_t = 4)]
    warmup: usize,

    /// Idle time between swaps. Some settle time is wanted — back-to-back
    /// swaps measure a runtime still busy tearing the previous flowgraph down,
    /// which is a different question — but it need not be long.
    #[arg(long, default_value_t = 50)]
    settle_ms: u64,

    /// Print each swap's step breakdown as it happens.
    #[arg(long)]
    verbose: bool,

    #[arg(long, default_value = "softswap_latency.csv")]
    out: PathBuf,

    /// 802.11ah flow to swap into. Defaults to the 9-block halow_rxA; pass
    /// flows/halowv6A.toml to measure the v6 receiver the dual-PHY captures
    /// and the replay bench actually run.
    #[arg(long, default_value = FLOW_H)]
    flow_h: PathBuf,

    /// The other flow of the pair. Defaults to the Zigbee receiver, giving the
    /// cross-PHY swap this binary was written for; set it to the A-side of one
    /// PHY (with --flow-h its B-side) to measure a same-PHY swap instead —
    /// `--flow-z flows/halowv6A.toml --flow-h flows/halowv6B.toml` is the swap
    /// `halow_swap` performs, and is what the recorded halow_swapv6*.csv
    /// captures time on real hardware.
    #[arg(long, default_value = FLOW_Z)]
    flow_z: PathBuf,
}

/// `[(name, [values])]` in step order, plus the totals — the shape the summary
/// wants, built once from the rows.
fn columns(rows: &[SwapTimings]) -> Vec<(&'static str, Vec<f64>)> {
    let mut out: Vec<(&'static str, Vec<f64>)> = SwapTimings::STEPS
        .iter()
        .map(|&name| (name, Vec::with_capacity(rows.len())))
        .collect();
    for row in rows {
        for (slot, value) in out.iter_mut().zip(row.as_row()) {
            // -1 marks a step that did not run at all; leaving it out keeps it
            // from being averaged in as a real measurement of zero.
            if value >= 0.0 {
                slot.1.push(value);
            }
        }
    }
    // Rollups last, and labelled: they contain the leaves above and must not
    // be read as another step alongside them.
    out.push(("build_and_start*", rows.iter().map(|r| r.build_and_start).collect()));
    out.push(("total*", rows.iter().map(|r| r.total).collect()));
    out
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = q * (sorted.len() - 1) as f64;
    let (lo, frac) = (pos.floor() as usize, pos.fract());
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

fn summarise(rows: &[SwapTimings]) {
    println!(
        "\n{:<21}{:>6}{:>10}{:>10}{:>10}{:>10}{:>10}",
        "step", "n", "median", "mean", "p95", "min", "max"
    );
    for (name, mut values) in columns(rows) {
        if values.is_empty() {
            println!("{name:<21}{:>6}{:>10}", 0, "not run");
            continue;
        }
        values.sort_by(f64::total_cmp);
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        println!(
            "{name:<21}{:>6}{:>10.3}{:>10.3}{:>10.3}{:>10.3}{:>10.3}",
            values.len(),
            quantile(&values, 0.5),
            mean,
            quantile(&values, 0.95),
            values[0],
            values[values.len() - 1],
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    futuresdr::runtime::init();

    println!("=== softswap_latency — swap cost with no radio in the loop ===");
    println!("head:  {HEAD_FLOW} (NullSource<Complex32>, no sdr_block)");
    let flow_h: String = args.flow_h.to_string_lossy().into_owned();
    let flow_z: String = args.flow_z.to_string_lossy().into_owned();
    if flow_h == flow_z {
        return Err("--flow-z and --flow-h must differ; a swap needs two flows".into());
    }
    println!("swaps: {flow_z} <-> {flow_h}");
    println!(
        "{} measured swaps after {} warmup, {} ms apart",
        args.swaps, args.warmup, args.settle_ms
    );
    println!("CSV → {}\n", args.out.display());

    let settle = Duration::from_millis(args.settle_ms);
    let builder = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_permanent(TAIL_FLOW)
        .add_swappable(flow_z.clone());

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        for &(idx, ref path, perm) in &entries {
            if perm {
                println!("Starting permanent fg/{idx}/ from '{path}'");
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                println!("Starting swappable fg/{idx}/ from '{path}'");
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        // The per-step commentary costs more than several of the steps it
        // reports, so measuring with it on would mostly measure stdout.
        ctrl.set_swap_verbose(args.verbose);

        let swap_target = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("at least one swappable flowgraph");

        let mut csv = BufWriter::new(File::create(&args.out)?);
        writeln!(
            csv,
            "swap_idx,from,to,{},build_and_start,total,t_ms",
            SwapTimings::STEPS.join(",")
        )?;

        let mut current: String = flow_z.clone();
        let mut rows: Vec<SwapTimings> = Vec::with_capacity(args.swaps);
        let t0 = Instant::now();

        println!("\nSwapping...");
        for i in 0..(args.warmup + args.swaps) {
            Timer::after(settle).await;

            let next: String = if current == flow_z {
                flow_h.clone()
            } else {
                flow_z.clone()
            };
            let timings = match ctrl.swap_timed(swap_target, &next, &rt_handle).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[swap {i}] failed: {e}");
                    continue;
                }
            };
            let from = current.clone();
            current = next;

            if i < args.warmup {
                continue;
            }
            let idx = i - args.warmup;

            let cells: Vec<String> = timings
                .as_row()
                .iter()
                .map(|v| format!("{v:.4}"))
                .collect();
            writeln!(
                csv,
                "{idx},{},{},{},{:.4},{:.4},{:.3}",
                side(&from, &flow_z),
                side(&current, &flow_z),
                cells.join(","),
                timings.build_and_start,
                timings.total,
                t0.elapsed().as_secs_f64() * 1000.0,
            )?;
            rows.push(timings);

            if (idx + 1) % 25 == 0 {
                println!("  {} / {} swaps", idx + 1, args.swaps);
            }
        }
        csv.flush()?;

        if rows.is_empty() {
            return Err("every swap failed — nothing measured".into());
        }
        summarise(&rows);
        println!("\nwrote {} ({} swaps)", args.out.display(), rows.len());

        ctrl.shutdown_all().await;

        // Step 6 of every swap hands the outgoing flowgraph to a detached
        // thread, so at this point there may still be threads running inside
        // code that belongs to a dlopen'd plugin. Returning normally from here
        // drops the PluginRegistry, which dlcloses those libraries out from
        // under them — a segfault at exit, reproducible and unrelated to any
        // measurement, which every other binary here avoids only by never
        // returning (they run until Ctrl-C). Give the stragglers a moment and
        // then leave without running destructors.
        Timer::after(Duration::from_millis(200)).await;
        std::process::exit(0);
    })
}

/// Which side of the configured pair a flow is: `Z` for `--flow-z`, `H` for
/// `--flow-h`. With the defaults those are the two PHYs, which is where the
/// letters come from; for a same-PHY run they are just the A and B sides, and
/// the CSV header plus the run's banner say which TOML each one was.
fn side(toml: &str, flow_z: &str) -> &'static str {
    if toml == flow_z { "Z" } else { "H" }
}
