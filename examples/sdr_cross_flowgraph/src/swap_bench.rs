// Cross-flowgraph swap benchmark (RadioController edition)
//
// Measures the time to hot-swap a swappable flowgraph between different
// receiver configurations (e.g. WLAN ↔ Zigbee ↔ discard).
//
// Architecture:
//   RadioController (permanent):
//     SeifySource → FirResampler → BridgeSink → [shared buf]
//
//   FlowgraphController (swappable):
//     [shared buf] → BridgeSource → protocol DSP
//
// The swap operation includes:
//   1. Terminate old flowgraph
//   2. Clear bridge buffers
//   3. Change radio bandwidth (rebuild resampler) + retune frequency
//   4. Build new flowgraph (load TOML, instantiate plugin blocks, wire bridges)
//   5. Start new flowgraph
//
// Usage:
//   cargo build -p sdr-cross-flowgraph-example \
//     -p seify_source_plugin -p fir_resampler_plugin \
//     -p null_sink_plugin ... (all protocol plugins)
//
//   cd examples/sdr_cross_flowgraph
//   ../../target/debug/swap_bench [iterations]

use plugin_host::{FlowgraphController, RadioController, default_plugin_dir};
use std::time::Instant;

/// Radio parameters for each swap target, parsed from TOML [radio] sections.
struct SwapTarget {
    toml_path: &'static str,
    frequency_hz: f64,
}

const SWAP_TARGETS: &[SwapTarget] = &[
    SwapTarget { toml_path: "flows/wlan_rx.toml",   frequency_hz: 2.475e9 },
    SwapTarget { toml_path: "flows/zigbee_rx.toml",  frequency_hz: 2.475e9 },
];

// The entire radio chain (SeifySource + FirResampler + BridgeSink) is built
// once and kept alive across all swaps. Both protocols consume IQ at
// OUTPUT_RATE; protocols that need a different rate (e.g. Zigbee at 4 Msps)
// prepend their own resampler inside their flowgraph TOML.
const HARDWARE_RATE: f64 = 40e6;
const OUTPUT_RATE:   f64 = 20e6;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    let iterations: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let plugin_dir = default_plugin_dir();

    println!("=== Cross-Flowgraph Swap Benchmark (RadioController) ===");
    println!("Iterations:    {iterations}");
    println!("Hardware rate: {} Msps", HARDWARE_RATE / 1e6);
    println!("Swap targets:  {:?}", SWAP_TARGETS.iter().map(|t| t.toml_path).collect::<Vec<_>>());
    println!();

    // Create RadioController — SDR at HARDWARE_RATE, resampler decimates to OUTPUT_RATE.
    // The chain is fixed for the lifetime of the controller; swaps only change frequency.
    let first = &SWAP_TARGETS[0];
    let (mut radio, radio_buf) = RadioController::new(
        &plugin_dir,
        "",
        first.frequency_hz,
        HARDWARE_RATE,
        OUTPUT_RATE,
        40.0,
    );

    // FlowgraphController with one swappable FG, fed from RadioController
    FlowgraphController::builder(&plugin_dir)
        .add_swappable(first.toml_path)          // fg/0/
        .connect_radio(radio_buf, 0, "in")
        .run_with(move |mut ctrl, rt_handle, entries| async move {
            // Pre-load plugins for all swap targets
            for target in SWAP_TARGETS {
                let content = std::fs::read_to_string(target.toml_path)?;
                let def: toml::Value = toml::from_str(&content)?;
                if let Some(blocks) = def.get("blocks").and_then(|b| b.as_array()) {
                    for block in blocks {
                        if let Some(plugin) = block.get("plugin").and_then(|p| p.as_str()) {
                            ctrl.registry_mut().ensure_loaded(plugin)?;
                        }
                    }
                }
            }

            // Start RadioController
            println!("Starting RadioController (SDR @ {} Msps) ...", HARDWARE_RATE / 1e6);
            radio.start(&rt_handle).await?;
            println!("  radio running: freq={} MHz, bw={} Msps",
                radio.frequency() / 1e6, radio.output_rate() / 1e6);

            // Start swappable FG
            for &(idx, ref toml_path, _permanent) in &entries {
                println!("Starting swappable fg/{idx}/ from '{toml_path}' ...");
                ctrl.start_swappable(idx, toml_path, &rt_handle).await?;
            }
            println!("All flowgraphs running.\n");

            // Let things settle
            futuresdr::async_io::Timer::after(std::time::Duration::from_secs(2)).await;

            // ── Benchmark loop ──
            let n_targets = SWAP_TARGETS.len();
            let total_swaps = iterations * n_targets;
            let mut times: Vec<(String, f64)> = Vec::with_capacity(total_swaps);

            println!("Starting {total_swaps} swaps ({iterations} × {n_targets} targets)...\n");

            for iter in 0..iterations {
                for (t_idx, target) in SWAP_TARGETS.iter().enumerate() {
                    let label = format!(
                        "[{}/{}] → {}",
                        iter * n_targets + t_idx + 1,
                        total_swaps,
                        target.toml_path
                    );

                    let t = Instant::now();

                    // Radio retune (a live message to the running SeifySource)
                    // runs concurrently with the protocol-FG swap. The radio
                    // chain itself (SeifySource + FirResampler + BridgeSink)
                    // is NOT rebuilt — it stays live across every swap.
                    let radio_fut = async {
                        let t_radio = Instant::now();
                        radio.set_frequency(target.frequency_hz).await?;
                        println!("    [radio] set_frequency:    {:.3} ms", t_radio.elapsed().as_secs_f64() * 1000.0);
                        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                    };
                    let swap_fut = async {
                        ctrl.swap(0, target.toml_path, &rt_handle).await?;
                        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                    };
                    let (r1, r2) = futuresdr::futures::join!(radio_fut, swap_fut);
                    r1?;
                    r2?;

                    let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;

                    println!("  {label}: {elapsed_ms:.2} ms (radio+swap combined)");
                    times.push((target.toml_path.to_string(), elapsed_ms));

                    // Settle between swaps
                    futuresdr::async_io::Timer::after(
                        std::time::Duration::from_secs(4),
                    ).await;
                }
            }

            // ── Summary ──
            println!();
            println!("╔══════════════════════════════════════════════════════════════════╗");
            println!("║          SWAP BENCHMARK RESULTS (RadioController)               ║");
            println!("╠══════════════════════════════════════════════════════════════════╣");
            println!(
                "║  {:30}  {:>8}  {:>8}  {:>8}  {:>6} ║",
                "Target", "Avg(ms)", "Min(ms)", "Max(ms)", "σ(ms)"
            );
            println!("╠══════════════════════════════════════════════════════════════════╣");

            for target in SWAP_TARGETS {
                let t_times: Vec<f64> = times
                    .iter()
                    .filter(|(name, _)| name == target.toml_path)
                    .map(|(_, ms)| *ms)
                    .collect();
                if t_times.is_empty() {
                    continue;
                }
                let avg = t_times.iter().sum::<f64>() / t_times.len() as f64;
                let min = t_times.iter().cloned().fold(f64::INFINITY, f64::min);
                let max = t_times.iter().cloned().fold(0.0f64, f64::max);
                let var = t_times.iter().map(|t| (t - avg).powi(2)).sum::<f64>()
                    / t_times.len() as f64;
                let sd = var.sqrt();

                let short = target.toml_path.rsplit('/').next().unwrap_or(target.toml_path);
                println!(
                    "║  {:30}  {:>8.2}  {:>8.2}  {:>8.2}  {:>6.2} ║",
                    short, avg, min, max, sd
                );
            }

            // Overall
            let all_ms: Vec<f64> = times.iter().map(|(_, ms)| *ms).collect();
            let avg = all_ms.iter().sum::<f64>() / all_ms.len() as f64;
            let min = all_ms.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = all_ms.iter().cloned().fold(0.0f64, f64::max);
            let var =
                all_ms.iter().map(|t| (t - avg).powi(2)).sum::<f64>() / all_ms.len() as f64;
            let sd = var.sqrt();
            println!("╠══════════════════════════════════════════════════════════════════╣");
            println!(
                "║  {:30}  {:>8.2}  {:>8.2}  {:>8.2}  {:>6.2} ║",
                "ALL SWAPS", avg, min, max, sd
            );
            println!("╚══════════════════════════════════════════════════════════════════╝");

            // ── Write CSV ──
            let mut csv = String::from("iteration,swap,target,time_ms\n");
            for (i, (target, ms)) in times.iter().enumerate() {
                let iter = i / n_targets;
                let swap = i % n_targets;
                let short = target.rsplit('/').next().unwrap_or(target);
                csv.push_str(&format!("{iter},{swap},{short},{ms:.4}\n"));
            }
            let csv_path = "swap_bench.csv";
            std::fs::write(csv_path, &csv)?;
            println!("\nTimings saved to {csv_path}");

            radio.shutdown().await;
            ctrl.shutdown_all().await;
            Ok(())
        })
}
