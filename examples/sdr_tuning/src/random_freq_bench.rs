// Random Frequency Change Benchmark — Seify drivers (soapy vs bladerf1)
//
// N random frequency hops across bladeRF 2.0 range (70 MHz – 5.9 GHz).
//
// Output: sdr_random_freq_bench.csv
//   method,iteration,from_hz,to_hz,time_us

use anyhow::Result;
use clap::Parser;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::NullSink;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use rand::Rng;
use std::time::Instant;

const FREQ_MIN: f64 = 70e6;
const FREQ_MAX: f64 = 5900e6;
const NUM_FREQS: usize = 200;

#[derive(Parser, Debug)]
#[command(name = "sdr-random-freq-bench")]
struct Args {
    /// Number of iterations per driver
    #[clap(short, long, default_value_t = 1000)]
    iterations: usize,

    #[clap(long, default_value = "driver=soapy")]
    soapy_args: String,

    #[clap(long, default_value = "driver=bladerf1")]
    bladerf1_args: String,

    #[clap(long, default_value_t = 4e6)]
    sample_rate: f64,

    #[clap(long, default_value_t = 40.0)]
    gain: f64,

    /// soapy, bladerf1, all
    #[clap(short, long, default_value = "all")]
    mode: String,
}

struct BenchEntry {
    from_hz: f64,
    to_hz: f64,
    time_us: f64,
}

fn gen_freqs(n: usize) -> Vec<f64> {
    let mut rng = rand::rng();
    (0..n).map(|_| {
        let khz = rng.random_range((FREQ_MIN / 1e3) as u64..=(FREQ_MAX / 1e3) as u64);
        khz as f64 * 1e3
    }).collect()
}

fn gen_indices(n: usize, max: usize) -> Vec<usize> {
    let mut rng = rand::rng();
    (0..n).map(|_| rng.random_range(0..max)).collect()
}

async fn bench_driver(
    label: String,
    mut handle: FlowgraphHandle,
    src_id: BlockId,
    freqs: Vec<f64>,
    indices: Vec<usize>,
    n: usize,
) -> (String, Vec<BenchEntry>) {
    handle.callback(src_id, "freq", Pmt::F64(freqs[0])).await.unwrap();
    futuresdr::async_io::Timer::after(std::time::Duration::from_millis(100)).await;

    let mut entries = Vec::with_capacity(n);
    let mut prev = 0usize;
    let mut sum = 0.0f64;

    for i in 0..n {
        let next = indices[i % indices.len()];
        let from_hz = freqs[prev];
        let to_hz = freqs[next];

        let t = Instant::now();
        handle.callback(src_id, "freq", Pmt::F64(to_hz)).await.unwrap();
        let us = t.elapsed().as_secs_f64() * 1e6;

        entries.push(BenchEntry { from_hz, to_hz, time_us: us });
        sum += us;
        prev = next;

        if (i + 1) % 100 == 0 {
            eprint!("  {label}: {}/{n} (avg={:.0} µs)\r", i + 1, sum / (i + 1) as f64);
        }
    }
    eprintln!();
    println!("  {label}: {n} iterations, avg={:.1} µs", sum / n as f64);

    handle.terminate_and_wait().await.unwrap();
    (label, entries)
}

fn run_driver(
    label: &str,
    device_args: &str,
    freqs: &[f64],
    indices: &[usize],
    n: usize,
    sample_rate: f64,
    gain: f64,
) -> Result<(String, Vec<BenchEntry>)> {
    println!("── {label} (args=\"{device_args}\") ──");
    let mut fg = Flowgraph::new();
    let src = Builder::new(device_args)?
        .frequency(freqs[0])
        .sample_rate(sample_rate)
        .gain(gain)
        .build_source()?;
    let src_ref = fg.add_block(src);
    let src_id: BlockId = (&src_ref).into();
    let snk = fg.add_block(NullSink::<Complex32>::new());
    fg.connect_stream(&mut src_ref.get()?.outputs()[0], &mut snk.get()?.input());

    let rt = Runtime::new();
    let (_task, handle) = rt.start_sync(fg)?;

    Ok(rt.block_on(bench_driver(
        label.to_string(), handle, src_id,
        freqs.to_vec(), indices.to_vec(), n,
    )))
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();
    let n = args.iterations;

    println!("=== Random Freq Benchmark ({n} iterations/driver) ===\n");

    let freqs = gen_freqs(NUM_FREQS);
    let indices = gen_indices(n, NUM_FREQS);
    let mut all: Vec<(String, Vec<BenchEntry>)> = Vec::new();

    if args.mode == "soapy" || args.mode == "all" {
        all.push(run_driver("soapy", &args.soapy_args, &freqs, &indices, n, args.sample_rate, args.gain)?);
    }
    if args.mode == "bladerf1" || args.mode == "all" {
        all.push(run_driver("bladerf1", &args.bladerf1_args, &freqs, &indices, n, args.sample_rate, args.gain)?);
    }

    let csv_path = "sdr_random_freq_bench.csv";
    let mut csv = String::from("method,iteration,from_hz,to_hz,time_us\n");
    for (label, entries) in &all {
        for (i, e) in entries.iter().enumerate() {
            csv.push_str(&format!("{},{},{:.0},{:.0},{:.1}\n", label, i, e.from_hz, e.to_hz, e.time_us));
        }
    }
    std::fs::write(csv_path, &csv)?;
    println!("\nCSV written to {csv_path}");
    Ok(())
}
