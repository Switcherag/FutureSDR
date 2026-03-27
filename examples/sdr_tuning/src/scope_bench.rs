// Oscilloscope-assisted SDR sample rate change benchmark
//
// Transmits a continuous CW tone via SeifySink and performs sample rate changes.
// An oscilloscope monitoring the RF output will see:
//   1. Stable CW carrier at center_freq + tone_offset
//   2. RF dropout when set_sample_rate() blocks the work loop
//   3. RF resumes when DAC restarts at the new rate
//
// The RF dropout duration measured on the oscilloscope is the TRUE full-stack
// latency: FutureSDR → seify → SoapySDR → libiio/libbladeRF → USB → firmware →
// kernel driver → AD9361 recalibration → PLL relock → DAC restart → first sample out.
//
// Compare with the software-measured time (handle.callback round-trip) to isolate
// hardware settling + DAC pipeline latency.

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::signal_source::SignalSourceBuilder;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "scope-rate-bench",
    about = "Oscilloscope-assisted sample rate change benchmark"
)]
struct Args {
    /// Seify device args (e.g. "driver=bladerf", "" for auto)
    #[clap(short, long, default_value = "")]
    args: String,

    /// TX center frequency (Hz)
    #[clap(long, default_value_t = 2.437e9)]
    freq: f64,

    /// Sample rate A (Hz) — starting rate
    #[clap(long, default_value_t = 4e6)]
    rate_a: f64,

    /// Sample rate B (Hz) — target rate
    #[clap(long, default_value_t = 20e6)]
    rate_b: f64,

    /// TX gain (dB)
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,

    /// CW tone offset from center (Hz) — sets the visible spectral line
    #[clap(long, default_value_t = 100_000.0)]
    tone_offset: f64,

    /// CW tone amplitude (0.0–1.0)
    #[clap(long, default_value_t = 0.8)]
    amplitude: f32,

    /// Number of A→B→A round-trip iterations
    #[clap(short, long, default_value_t = 5)]
    iterations: usize,

    /// Seconds of stable TX before each rate change (scope settle time)
    #[clap(long, default_value_t = 4.0)]
    settle: f64,

    /// Seconds between round-trips
    #[clap(long, default_value_t = 3.0)]
    pause: f64,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();

    println!("=== Oscilloscope Sample-Rate Benchmark ===");
    println!("Device:       {:?}", args.args);
    println!("TX frequency: {:.3} MHz", args.freq / 1e6);
    println!("Tone offset:  {:.0} kHz  (carrier at {:.3} MHz)",
        args.tone_offset / 1e3, (args.freq + args.tone_offset as f64) / 1e6);
    println!("Rate A:       {:.3} MSPS", args.rate_a / 1e6);
    println!("Rate B:       {:.3} MSPS", args.rate_b / 1e6);
    println!("Gain:         {:.1} dB", args.gain);
    println!("Amplitude:    {:.2}", args.amplitude);
    println!("Iterations:   {}", args.iterations);
    println!("Settle time:  {:.1}s", args.settle);
    println!();

    // ── Build flowgraph: SignalSource(CW) → SeifySink ──
    let mut fg = Flowgraph::new();

    let sig_src = SignalSourceBuilder::<Complex32>::sin(
        args.tone_offset as f32,
        args.rate_a as f32,
        args.amplitude,
        0.0,
    );
    let src = fg.add_block(sig_src);

    let snk = Builder::new(&args.args)?
        .frequency(args.freq)
        .sample_rate(args.rate_a)
        .gain(args.gain)
        .build_sink()?;
    let snk_ref = fg.add_block(snk);
    let snk_id: BlockId = (&snk_ref).into();
    fg.connect_stream(src.get()?.output(), &mut snk_ref.get()?.inputs()[0]);

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    println!("Flowgraph started — CW tone transmitting at {:.3} MSPS.", args.rate_a / 1e6);
    println!();
    println!("Oscilloscope setup:");
    println!("  1. Tune receiver/probe to {:.3} MHz", args.freq / 1e6);
    println!("  2. Set timebase to ~200 ms/div (captures full dropout)");
    println!("  3. Trigger: falling edge on RF amplitude");
    println!("  4. Use SINGLE-SHOT trigger mode");
    println!("  5. Measure the dropout gap with cursors → that is the true full-stack latency");
    println!();
    println!("Starting in {:.0}s (settle time for oscilloscope)...", args.settle);

    let settle = Duration::from_secs_f64(args.settle);
    let pause = Duration::from_secs_f64(args.pause);
    let t0 = Instant::now();

    rt.block_on(async move {
        Timer::after(settle).await;

        let mut sw_times_a_to_b: Vec<f64> = Vec::new();
        let mut sw_times_b_to_a: Vec<f64> = Vec::new();

        for i in 0..args.iterations {
            println!("──── Iteration {}/{} ────", i + 1, args.iterations);

            // ── A → B ──
            println!("  [{:>8.1}s] Stable at {:.1} MSPS. Changing to {:.1} MSPS in 3…",
                t0.elapsed().as_secs_f64(), args.rate_a / 1e6, args.rate_b / 1e6);
            Timer::after(Duration::from_secs(1)).await;
            println!("  [{:>8.1}s] 2…", t0.elapsed().as_secs_f64());
            Timer::after(Duration::from_secs(1)).await;
            println!("  [{:>8.1}s] 1…", t0.elapsed().as_secs_f64());
            Timer::after(Duration::from_secs(1)).await;

            let t = Instant::now();
            println!("  [{:>8.1}s] >>> set_sample_rate({:.1} MSPS) — RF will drop NOW",
                t0.elapsed().as_secs_f64(), args.rate_b / 1e6);
            handle.callback(snk_id, "sample_rate", Pmt::F64(args.rate_b)).await.unwrap();
            let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;
            println!("  [{:>8.1}s] <<< Software returned: {:.2} ms",
                t0.elapsed().as_secs_f64(), elapsed_ms);
            sw_times_a_to_b.push(elapsed_ms);

            // Settle at new rate
            Timer::after(settle).await;

            // ── B → A ──
            println!("  [{:>8.1}s] Stable at {:.1} MSPS. Changing back to {:.1} MSPS in 3…",
                t0.elapsed().as_secs_f64(), args.rate_b / 1e6, args.rate_a / 1e6);
            Timer::after(Duration::from_secs(1)).await;
            println!("  [{:>8.1}s] 2…", t0.elapsed().as_secs_f64());
            Timer::after(Duration::from_secs(1)).await;
            println!("  [{:>8.1}s] 1…", t0.elapsed().as_secs_f64());
            Timer::after(Duration::from_secs(1)).await;

            let t = Instant::now();
            println!("  [{:>8.1}s] >>> set_sample_rate({:.1} MSPS) — RF will drop NOW",
                t0.elapsed().as_secs_f64(), args.rate_a / 1e6);
            handle.callback(snk_id, "sample_rate", Pmt::F64(args.rate_a)).await.unwrap();
            let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;
            println!("  [{:>8.1}s] <<< Software returned: {:.2} ms",
                t0.elapsed().as_secs_f64(), elapsed_ms);
            sw_times_b_to_a.push(elapsed_ms);

            if i + 1 < args.iterations {
                println!("  Pausing {:.0}s before next iteration...", args.pause);
                Timer::after(pause).await;
            }
        }

        // ── Summary ──
        println!();
        println!("╔══════════════════════════════════════════════════════════════╗");
        println!("║           SOFTWARE-MEASURED RESULTS (callback RTT)          ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║                                                              ║");
        print_summary(&format!("{:.1} → {:.1} MSPS", args.rate_a / 1e6, args.rate_b / 1e6),
            &sw_times_a_to_b);
        print_summary(&format!("{:.1} → {:.1} MSPS", args.rate_b / 1e6, args.rate_a / 1e6),
            &sw_times_b_to_a);
        println!("║                                                              ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  Compare with oscilloscope-measured RF dropout duration.    ║");
        println!("║                                                              ║");
        println!("║  If scope_time > software_time:                             ║");
        println!("║    → extra time = HW settling + DAC pipeline refill         ║");
        println!("║                                                              ║");
        println!("║  If scope_time ≈ software_time:                             ║");
        println!("║    → software stack dominates (USB round-trips)             ║");
        println!("╚══════════════════════════════════════════════════════════════╝");

        // Write CSV for later analysis
        let mut csv = String::from("direction,iteration,sw_time_ms\n");
        for (i, t) in sw_times_a_to_b.iter().enumerate() {
            csv.push_str(&format!("a_to_b,{},{:.4}\n", i, t));
        }
        for (i, t) in sw_times_b_to_a.iter().enumerate() {
            csv.push_str(&format!("b_to_a,{},{:.4}\n", i, t));
        }
        let csv_path = "scope_rate_bench.csv";
        std::fs::write(csv_path, &csv).unwrap();
        println!("\nSoftware timings saved to {csv_path}");
        println!("Add oscilloscope measurements to complete the comparison.");

        handle.terminate_and_wait().await.unwrap();
    });

    Ok(())
}

fn print_summary(label: &str, times: &[f64]) {
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = times.iter().cloned().fold(0.0f64, f64::max);
    let var = times.iter().map(|t| (t - avg).powi(2)).sum::<f64>() / times.len() as f64;
    let sd = var.sqrt();
    println!("║  {:<20} avg={:>8.2}ms  min={:>8.2}ms  max={:>8.2}ms  σ={:.2}ms",
        label, avg, min, max, sd);
}
