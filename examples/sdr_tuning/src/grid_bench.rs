// SDR Grid Benchmark — 10×10 transition matrix for sample rate and frequency
//
// Measures every (from → to) transition through FutureSDR → seify → SoapySDR.
// Same grid as bladerf_grid_bench.c for direct comparison.
//
// Run with --args "" for auto-detect, or --args "driver=bladerf" / "driver=plutosdr"
// Output: sdr_grid_bench.csv  (rename per device for comparison)

use anyhow::Result;
use clap::Parser;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::NullSource;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::time::Instant;

const RATES_MHZ: [u32; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
const FREQS_MHZ: [u32; 10] = [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];

#[derive(Parser, Debug)]
#[command(name = "sdr-grid-bench", about = "10x10 grid benchmark for SDR reconfiguration")]
struct Args {
    /// Seify device args (e.g. "driver=bladerf", "driver=plutosdr", "" for auto)
    #[clap(short, long, default_value = "")]
    args: String,

    /// Runs per grid cell
    #[clap(short = 'n', long, default_value_t = 5)]
    runs: usize,

    /// TX gain (dB)
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,

    /// Output CSV file
    #[clap(short, long, default_value = "sdr_grid_bench.csv")]
    output: String,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();

    println!("=== SDR Grid Benchmark (SoapySDR via FutureSDR) ===");
    println!("Device args:   {:?}", args.args);
    println!("Runs per cell: {}", args.runs);
    println!("Rate grid:     10x10  [1..10 MHz]");
    println!("Freq grid:     10x10  [100..1000 MHz]");
    println!("Total calls:   ~{}  (rate) + ~{}  (freq)",
        10 * 10 * args.runs * 2, 10 * 10 * args.runs * 2);
    println!();

    // Build flowgraph: NullSource → SeifySink
    let t_open = Instant::now();
    let mut fg = Flowgraph::new();
    let src = fg.add_block(NullSource::<Complex32>::new());
    let snk = Builder::new(&args.args)?
        .frequency(100e6)
        .sample_rate(1e6)
        .gain(args.gain)
        .build_sink()?;
    let snk_ref = fg.add_block(snk);
    let snk_id: BlockId = (&snk_ref).into();
    fg.connect_stream(src.get()?.output(), &mut snk_ref.get()?.inputs()[0]);
    println!("Device opened in {:.1}ms", t_open.elapsed().as_secs_f64() * 1000.0);

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;
    println!("Flowgraph started\n");

    let runs = args.runs;
    let output_path = args.output.clone();

    let csv_data = rt.block_on(async move {
        let mut csv = String::from("param,from,to,run,time_ms\n");
        let bench_start = Instant::now();

        // ── Sample Rate Grid ──
        println!("── Sample Rate Grid (TX, SoapySDR) ──");
        print!("from\\to ");
        for mhz in &RATES_MHZ { print!(" {:>5}M", mhz); }
        println!();

        for &from_mhz in &RATES_MHZ {
            print!(" {:>4}M  ", from_mhz);
            for &to_mhz in &RATES_MHZ {
                let mut total = 0.0;
                let mut ok = 0;
                for run in 0..runs {
                    // Reset to from_rate
                    let _ = handle.callback(
                        snk_id, "sample_rate", Pmt::F64(from_mhz as f64 * 1e6)
                    ).await;
                    // Measure transition
                    let t = Instant::now();
                    let result = handle.callback(
                        snk_id, "sample_rate", Pmt::F64(to_mhz as f64 * 1e6)
                    ).await;
                    let elapsed = t.elapsed().as_secs_f64() * 1000.0;
                    if result.is_ok() {
                        csv.push_str(&format!(
                            "rate,{},{},{},{:.4}\n", from_mhz, to_mhz, run, elapsed
                        ));
                        total += elapsed;
                        ok += 1;
                    } else {
                        csv.push_str(&format!(
                            "rate,{},{},{},-1\n", from_mhz, to_mhz, run
                        ));
                    }
                }
                let avg = if ok > 0 { total / ok as f64 } else { -1.0 };
                if avg >= 0.0 { print!(" {:>5.1}", avg); }
                else { print!("   ERR"); }
            }
            println!();
        }

        // Reset for freq grid
        let _ = handle.callback(snk_id, "sample_rate", Pmt::F64(4e6)).await;

        // ── Frequency Grid ──
        println!("\n── Frequency Grid (TX, SoapySDR, at 4 MSPS) ──");
        print!("from\\to  ");
        for mhz in &FREQS_MHZ { print!(" {:>5}M", mhz); }
        println!();

        for &from_mhz in &FREQS_MHZ {
            print!(" {:>5}M  ", from_mhz);
            for &to_mhz in &FREQS_MHZ {
                let mut total = 0.0;
                let mut ok = 0;
                for run in 0..runs {
                    // Reset to from_freq
                    let _ = handle.callback(
                        snk_id, "freq", Pmt::F64(from_mhz as f64 * 1e6)
                    ).await;
                    // Measure transition
                    let t = Instant::now();
                    let result = handle.callback(
                        snk_id, "freq", Pmt::F64(to_mhz as f64 * 1e6)
                    ).await;
                    let elapsed = t.elapsed().as_secs_f64() * 1000.0;
                    if result.is_ok() {
                        csv.push_str(&format!(
                            "freq,{},{},{},{:.4}\n", from_mhz, to_mhz, run, elapsed
                        ));
                        total += elapsed;
                        ok += 1;
                    } else {
                        csv.push_str(&format!(
                            "freq,{},{},{},-1\n", from_mhz, to_mhz, run
                        ));
                    }
                }
                let avg = if ok > 0 { total / ok as f64 } else { -1.0 };
                if avg >= 0.0 { print!(" {:>5.1}", avg); }
                else { print!("   ERR"); }
            }
            println!();
        }

        let total_secs = bench_start.elapsed().as_secs_f64();
        println!("\nTotal benchmark time: {:.1}s", total_secs);

        handle.terminate_and_wait().await.unwrap();
        csv
    });

    std::fs::write(&output_path, &csv_data)?;
    println!("CSV written to {output_path}");

    Ok(())
}
