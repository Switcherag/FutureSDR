// Cross-flowgraph swap benchmark (FlowgraphController only, no SDR)
//
// Measures the time to hot-swap between Flow A and Flow B.
//
// Architecture:
//   FG0 (permanent):  NullSource -> Throttle -> Selector -> [auto bridge u8] -> FG1
//   FG1 (swappable):  [auto bridge u8] -> PrintSink (flow_a or flow_b)
//
// Usage:
//   cargo build -p cross-flowgraph-example \
//     -p null_source_plugin -p throttle_plugin -p selector_1_2_plugin \
//     -p null_sink_plugin -p print_sink_plugin
//
//   cd examples/cross_flowgraph
//   ../../target/debug/cross_fg_bench [iterations]

use plugin_host::{FlowgraphController, default_plugin_dir};
use std::time::Instant;

const SWAP_TARGETS: &[&str] = &[
    "flows/flow_a.toml",
    "flows/flow_b.toml",
];

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    let iterations: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let plugin_dir = default_plugin_dir();

    println!("=== Cross-Flowgraph Swap Benchmark ===");
    println!("Iterations:   {iterations}");
    println!("Swap targets: {SWAP_TARGETS:?}");
    println!();

    FlowgraphController::builder(&plugin_dir)
        .add_permanent("flows/fg0.toml")          // fg/0/ — source + selector
        .add_swappable(SWAP_TARGETS[0])            // fg/1/ — start with flow_a
        .connect(0, "out", 1, "in")
        .run_with(move |mut ctrl, rt_handle, entries| async move {
            // Pre-load all plugins
            for target in SWAP_TARGETS {
                let content = std::fs::read_to_string(target)?;
                let def: toml::Value = toml::from_str(&content)?;
                if let Some(blocks) = def.get("blocks").and_then(|b| b.as_array()) {
                    for block in blocks {
                        if let Some(plugin) = block.get("plugin").and_then(|p| p.as_str()) {
                            ctrl.registry_mut().ensure_loaded(plugin)?;
                        }
                    }
                }
            }

            // Start permanent FG
            for &(idx, ref toml_path, permanent) in &entries {
                if permanent {
                    println!("Starting permanent fg/{idx}/ from '{toml_path}' ...");
                    ctrl.start_permanent(idx, toml_path, &rt_handle).await?;
                }
            }

            ctrl.activate_selectors().await?;

            // Start swappable FG
            for &(idx, ref toml_path, permanent) in &entries {
                if !permanent {
                    println!("Starting swappable fg/{idx}/ from '{toml_path}' ...");
                    ctrl.start_swappable(idx, toml_path, &rt_handle).await?;
                }
            }
            println!("All flowgraphs running.\n");

            // Let things settle
            futuresdr::async_io::Timer::after(std::time::Duration::from_secs(1)).await;

            // -- Benchmark loop --
            let n_targets = SWAP_TARGETS.len();
            let total_swaps = iterations * n_targets;
            let mut times: Vec<(String, f64)> = Vec::with_capacity(total_swaps);

            println!("Starting {total_swaps} swaps ({iterations} x {n_targets} targets)...\n");

            for iter in 0..iterations {
                for (t_idx, &target) in SWAP_TARGETS.iter().enumerate() {
                    let label = format!(
                        "[{}/{}] -> {}",
                        iter * n_targets + t_idx + 1,
                        total_swaps,
                        target
                    );

                    let t = Instant::now();
                    ctrl.swap(1, target, &rt_handle).await?;
                    let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;

                    println!("  {label}: {elapsed_ms:.2} ms");
                    times.push((target.to_string(), elapsed_ms));

                    // Brief settle
                    futuresdr::async_io::Timer::after(
                        std::time::Duration::from_millis(200),
                    ).await;
                }
            }

            // -- Summary --
            println!();
            println!("╔══════════════════════════════════════════════════════════════════╗");
            println!("║            SWAP BENCHMARK RESULTS (FlowgraphController)         ║");
            println!("╠══════════════════════════════════════════════════════════════════╣");
            println!(
                "║  {:30}  {:>8}  {:>8}  {:>8}  {:>6} ║",
                "Target", "Avg(ms)", "Min(ms)", "Max(ms)", "σ(ms)"
            );
            println!("╠══════════════════════════════════════════════════════════════════╣");

            for &target in SWAP_TARGETS {
                let t_times: Vec<f64> = times
                    .iter()
                    .filter(|(name, _)| name == target)
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

                let short = target.rsplit('/').next().unwrap_or(target);
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

            // -- Write CSV --
            let mut csv = String::from("iteration,swap,target,time_ms\n");
            for (i, (target, ms)) in times.iter().enumerate() {
                let iter = i / n_targets;
                let swap = i % n_targets;
                let short = target.rsplit('/').next().unwrap_or(target);
                csv.push_str(&format!("{iter},{swap},{short},{ms:.4}\n"));
            }
            let csv_path = "cross_fg_bench.csv";
            std::fs::write(csv_path, &csv)?;
            println!("\nTimings saved to {csv_path}");

            ctrl.shutdown_all().await;
            std::process::exit(0);
        })
}
