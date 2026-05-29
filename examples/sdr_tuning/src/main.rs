// SDR Tuning Benchmark
//
// Measures SoapySDR reconfiguration latency on running Seify blocks.
// Compares static (linked) vs dynamic (plugin .so) for both RX and TX.
//
// Modes: rx_static, tx_static, rx_dyn, tx_dyn, all

use anyhow::Result;
use clap::Parser;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::{NullSink, NullSource};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use plugin_host::LoadedPlugin;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "sdr-tuning-bench", about = "Benchmark SDR reconfiguration latency")]
struct Args {
    /// Seify device args (e.g. "driver=hackrf", "" for auto)
    #[clap(short, long, default_value = "")]
    args: String,

    /// Gain in dB
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,

    /// Enable RX AGC on Seify source blocks
    #[clap(long, default_value_t = false)]
    agc: bool,

    /// Base frequency (Hz)
    #[clap(long, default_value_t = 2.437e9)]
    base_freq: f64,

    /// Base sample rate (Hz)
    #[clap(long, default_value_t = 4e6)]
    base_rate: f64,

    /// Number of iterations per test
    #[clap(short, long, default_value_t = 20)]
    iterations: usize,

    /// Mode: rx_static, tx_static, rx_dyn, tx_dyn, or all
    #[clap(short, long, default_value = "all")]
    mode: String,

    /// Directory containing plugin .so files (for dyn modes)
    #[clap(long, default_value = ".")]
    plugin_dir: String,
}

#[derive(Clone)]
struct BenchResult {
    name: String,
    times_ms: Vec<f64>,
}

impl BenchResult {
    fn avg(&self) -> f64 {
        self.times_ms.iter().sum::<f64>() / self.times_ms.len() as f64
    }
    fn min(&self) -> f64 {
        self.times_ms.iter().cloned().fold(f64::INFINITY, f64::min)
    }
    fn max(&self) -> f64 {
        self.times_ms.iter().cloned().fold(0.0f64, f64::max)
    }
    fn std_dev(&self) -> f64 {
        let avg = self.avg();
        let var = self.times_ms.iter().map(|t| (t - avg).powi(2)).sum::<f64>()
            / self.times_ms.len() as f64;
        var.sqrt()
    }
}

/// Run 7 benchmarks on a seify block inside a started flowgraph.
/// Returns results.  The handle is consumed (flowgraph terminated).
async fn bench_suite(
    label: &str,
    mut handle: FlowgraphHandle,
    seify_id: BlockId,
    freq_a: f64,
    freq_b: f64,
    rate_a: f64,
    rate_b: f64,
    gain_a: f64,
    gain_b: f64,
    n: usize,
) -> Vec<BenchResult> {
    let mut results: Vec<BenchResult> = Vec::new();

    // 1: Frequency retune
    {
        print!("  [1/7] Frequency ({:.1} <-> {:.1} MHz)... ", freq_a / 1e6, freq_b / 1e6);
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let target = if i % 2 == 0 { freq_b } else { freq_a };
            let t = Instant::now();
            handle.callback(seify_id, "freq", Pmt::F64(target)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_freq"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    // 2: Sample rate
    {
        handle.callback(seify_id, "freq", Pmt::F64(freq_a)).await.unwrap();
        print!("  [2/7] Sample rate ({:.1} <-> {:.1} MHz)... ", rate_a / 1e6, rate_b / 1e6);
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let target = if i % 2 == 0 { rate_b } else { rate_a };
            let t = Instant::now();
            handle.callback(seify_id, "sample_rate", Pmt::F64(target)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_sample_rate"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    // 3: Gain
    {
        handle.callback(seify_id, "sample_rate", Pmt::F64(rate_a)).await.unwrap();
        print!("  [3/7] Gain ({:.1} <-> {:.1} dB)... ", gain_a, gain_b);
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let target = if i % 2 == 0 { gain_b } else { gain_a };
            let t = Instant::now();
            handle.callback(seify_id, "gain", Pmt::F64(target)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_gain"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    // 4: Freq + rate
    {
        print!("  [4/7] Freq + rate (sequential)... ");
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let (f, sr) = if i % 2 == 0 { (freq_b, rate_b) } else { (freq_a, rate_a) };
            let t = Instant::now();
            handle.callback(seify_id, "freq", Pmt::F64(f)).await.unwrap();
            handle.callback(seify_id, "sample_rate", Pmt::F64(sr)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_freq+rate"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    // 5: Freq + rate + gain
    {
        print!("  [5/7] Freq + rate + gain... ");
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let (f, sr, g) = if i % 2 == 0 {
                (freq_b, rate_b, gain_b)
            } else {
                (freq_a, rate_a, gain_a)
            };
            let t = Instant::now();
            handle.callback(seify_id, "freq", Pmt::F64(f)).await.unwrap();
            handle.callback(seify_id, "sample_rate", Pmt::F64(sr)).await.unwrap();
            handle.callback(seify_id, "gain", Pmt::F64(g)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_freq+rate+gain"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    // 6: Same freq (no-op)
    {
        handle.callback(seify_id, "freq", Pmt::F64(freq_a)).await.unwrap();
        handle.callback(seify_id, "sample_rate", Pmt::F64(rate_a)).await.unwrap();
        print!("  [6/7] Same freq (no-op)... ");
        let mut times = Vec::with_capacity(n);
        for _ in 0..n {
            let t = Instant::now();
            handle.callback(seify_id, "freq", Pmt::F64(freq_a)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_freq_noop"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    // 7: Small freq hop (1 MHz)
    {
        print!("  [7/7] 1 MHz freq hop... ");
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let target = freq_a + (i as f64 % 5.0) * 1e6;
            let t = Instant::now();
            handle.callback(seify_id, "freq", Pmt::F64(target)).await.unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let r = BenchResult { name: format!("{label}_freq_1mhz"), times_ms: times };
        println!("avg={:.2}ms  min={:.2}ms  max={:.2}ms  sd={:.2}ms",
            r.avg(), r.min(), r.max(), r.std_dev());
        results.push(r);
    }

    handle.terminate_and_wait().await.unwrap();
    results
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();

    println!("SDR Tuning Benchmark");
    println!("====================");
    println!("Device args: {:?}", args.args);
    println!("Base freq:   {:.3} MHz", args.base_freq / 1e6);
    println!("Base rate:   {:.3} MHz", args.base_rate / 1e6);
    println!("Gain:        {:.1} dB", args.gain);
    println!("AGC:         {}", args.agc);
    println!("Iterations:  {}", args.iterations);
    println!("Mode:        {}", args.mode);
    println!();

    let freq_a = args.base_freq;
    let freq_b = args.base_freq + 43e6;
    let rate_a = args.base_rate;
    let rate_b = args.base_rate * 5.0;
    let gain_a = args.gain;
    let gain_b = (args.gain - 20.0).max(0.0);
    let n = args.iterations;
    let mode = &args.mode;

    let do_rx_static = mode == "rx_static" || mode == "all";
    let do_tx_static = mode == "tx_static" || mode == "all";
    let do_rx_dyn = mode == "rx_dyn" || mode == "all";
    let do_tx_dyn = mode == "tx_dyn" || mode == "all";

    let rt = Runtime::new();
    let mut all_results: Vec<BenchResult> = Vec::new();

    // ============================================================
    // RX Static: SeifySource → NullSink
    // ============================================================
    if do_rx_static {
        println!("=== RX Static (SeifySource → NullSink) ===");
        let t_open = Instant::now();
        let mut fg = Flowgraph::new();
        let src = Builder::new(&args.args)?
            .frequency(freq_a)
            .sample_rate(rate_a)
            .gain(gain_a)
            .build_source()?;
        let src_ref = fg.add_block(src);
        let src_id: BlockId = (&src_ref).into();
        let snk = fg.add_block(NullSink::<Complex32>::new());
        fg.connect_stream(&mut src_ref.get()?.outputs()[0], &mut snk.get()?.input());
        println!("  Device opened in {:.1}ms", t_open.elapsed().as_secs_f64() * 1000.0);

        let (_fg_task, mut handle) = rt.start_sync(fg)?;
        if args.agc {
            println!("  Enabling RX AGC...");
            rt.block_on(async {
                let _ = handle.callback(src_id, "agc", Pmt::Bool(true)).await;
            });
        }
        println!("  Flowgraph started\n");
        let res = rt.block_on(bench_suite(
            "rx_static", handle, src_id,
            freq_a, freq_b, rate_a, rate_b, gain_a, gain_b, n,
        ));
        all_results.extend(res);
        println!();
    }

    // ============================================================
    // TX Static: NullSource → SeifySink
    // ============================================================
    if do_tx_static {
        println!("=== TX Static (NullSource → SeifySink) ===");
        let t_open = Instant::now();
        let mut fg = Flowgraph::new();
        let src = fg.add_block(NullSource::<Complex32>::new());
        let snk = Builder::new(&args.args)?
            .frequency(freq_a)
            .sample_rate(rate_a)
            .gain(gain_a)
            .build_sink()?;
        let snk_ref = fg.add_block(snk);
        let snk_id: BlockId = (&snk_ref).into();
        fg.connect_stream(src.get()?.output(), &mut snk_ref.get()?.inputs()[0]);
        println!("  Device opened in {:.1}ms", t_open.elapsed().as_secs_f64() * 1000.0);

        let (_fg_task, handle) = rt.start_sync(fg)?;
        println!("  Flowgraph started\n");
        let res = rt.block_on(bench_suite(
            "tx_static", handle, snk_id,
            freq_a, freq_b, rate_a, rate_b, gain_a, gain_b, n,
        ));
        all_results.extend(res);
        println!();
    }

    // ============================================================
    // RX Dynamic: SeifySource plugin → NullSink
    // ============================================================
    if do_rx_dyn {
        let dir = &args.plugin_dir;
        println!("=== RX Dynamic (SeifySource plugin → NullSink) ===");
        let seify_src_plugin =
            unsafe { LoadedPlugin::load(&format!("{dir}/libseify_source_plugin.so")) };
        let t_open = Instant::now();
        let mut fg = Flowgraph::new();
        let src = fg.add_block_dyn(seify_src_plugin.prepare(Box::new((
            args.args.clone(), freq_a, rate_a, gain_a,
        ))));
        let src_id: BlockId = src;
        let snk = fg.add_block(NullSink::<Complex32>::new());
        let snk_id: BlockId = (&snk).into();
        fg.connect_dyn(src_id, "outputs[0]", snk_id, "input")?;
        println!("  Device opened in {:.1}ms", t_open.elapsed().as_secs_f64() * 1000.0);

        let (_fg_task, mut handle) = rt.start_sync(fg)?;
        if args.agc {
            println!("  Enabling RX AGC...");
            rt.block_on(async {
                let _ = handle.callback(src_id, "agc", Pmt::Bool(true)).await;
            });
        }
        println!("  Flowgraph started\n");
        let res = rt.block_on(bench_suite(
            "rx_dyn", handle, src_id,
            freq_a, freq_b, rate_a, rate_b, gain_a, gain_b, n,
        ));
        all_results.extend(res);
        println!();
    }

    // ============================================================
    // TX Dynamic: NullSource → SeifySink plugin
    // ============================================================
    if do_tx_dyn {
        let dir = &args.plugin_dir;
        println!("=== TX Dynamic (NullSource → SeifySink plugin) ===");
        let seify_snk_plugin =
            unsafe { LoadedPlugin::load(&format!("{dir}/libseify_sink_plugin.so")) };
        let t_open = Instant::now();
        let mut fg = Flowgraph::new();
        let src = fg.add_block(NullSource::<Complex32>::new());
        let src_id: BlockId = (&src).into();
        let snk = fg.add_block_dyn(seify_snk_plugin.prepare(Box::new((
            args.args.clone(), freq_a, rate_a, gain_a,
        ))));
        let snk_id: BlockId = snk;
        fg.connect_dyn(src_id, "output", snk_id, "inputs[0]")?;
        println!("  Device opened in {:.1}ms", t_open.elapsed().as_secs_f64() * 1000.0);

        let (_fg_task, handle) = rt.start_sync(fg)?;
        println!("  Flowgraph started\n");
        let res = rt.block_on(bench_suite(
            "tx_dyn", handle, snk_id,
            freq_a, freq_b, rate_a, rate_b, gain_a, gain_b, n,
        ));
        all_results.extend(res);
        println!();
    }

    // ============================================================
    // Summary table
    // ============================================================
    println!("{}", "=".repeat(95));
    println!("{:<35} {:>10} {:>10} {:>10} {:>10} {:>6}",
        "Test", "Avg (ms)", "Min (ms)", "Max (ms)", "StdDev", "N");
    println!("{}", "-".repeat(95));
    for r in &all_results {
        println!("{:<35} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>6}",
            r.name, r.avg(), r.min(), r.max(), r.std_dev(), r.times_ms.len());
    }
    println!("{}", "=".repeat(95));

    // ============================================================
    // Write CSV
    // ============================================================
    let csv_path = "sdr_tuning_bench.csv";
    let mut csv = String::from("test,iteration,time_ms\n");
    for r in &all_results {
        for (i, t) in r.times_ms.iter().enumerate() {
            csv.push_str(&format!("{},{},{:.4}\n", r.name, i, t));
        }
    }
    std::fs::write(csv_path, &csv)?;
    println!("\nDetailed timings written to {csv_path}");

    Ok(())
}
