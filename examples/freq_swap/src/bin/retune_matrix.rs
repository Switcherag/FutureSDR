// retune_matrix — time every channel-to-channel retune, Zigbee ↔ 802.11ah US.
//
// Answers "how long does set_frequency actually take, and does it depend on
// which pair of channels?" by measuring every ordered pair in one square
// matrix, so an in-band hop and a cross-band hop are measured by the same code
// in the same run and can be compared directly.
//
// Channel plans (centres in MHz):
//   Z   802.15.4 2.4 GHz, channels 11..26   f = 2405 + 5*(ch-11)   → 2405..2480
//   H   802.11ah US 1 MHz, channels 1..26   f = 902.5 + (ch-1)     → 902.5..927.5
//
// That is 42 channels, so 42x42 = 1764 ordered pairs. Each measurement is:
//   set_frequency(from) → settle → t0 → set_frequency(to) → t1
// and only the second call is timed, so the result is the cost of arriving at
// `to` *from* `from` rather than from wherever the previous pair left off.
//
// `--stream` additionally runs an RX stream during the sweep. This matters: the
// SoapySDR device cache means a second `Device::from_args` handle is generally
// the *same* underlying device, so a retune contends with the streaming
// thread's `read()`. If that is what dominates, `--stream` will be far slower
// than the idle case and the effect will scale with `--sample-rate` (a buffer
// at 4 MSps takes 5x as long to fill as at 20 MSps).
//
// Output: retune_matrix.csv — from_label,to_label,from_mhz,to_mhz,ms
// Plot:   python3 plot_retune_matrix.py
//
// Run from this directory:
//   cd examples/freq_swap && cargo run --release --bin retune_matrix -- --stream

use std::fs::File;
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use futuresdr::num_complex::Complex32;
use futuresdr::seify::{Device, Direction, RxStreamer};

#[derive(Parser, Debug)]
#[command(about = "Time every Zigbee <-> 802.11ah US channel retune into a square matrix.")]
struct Args {
    /// Device args for seify, e.g. "driver=bladerf". Empty = first available.
    #[arg(long, default_value = "")]
    device: String,
    /// Hardware sample rate to configure before sweeping (Hz).
    #[arg(long, default_value_t = 4e6)]
    sample_rate: f64,
    /// Run an RX stream during the sweep, so retunes contend with reads.
    #[arg(long)]
    stream: bool,
    /// Settle time after moving to the `from` channel, before timing (ms).
    #[arg(long, default_value_t = 20)]
    settle_ms: u64,
    /// Repeats per ordered pair; the median is reported.
    #[arg(long, default_value_t = 1)]
    repeat: usize,
    /// Output CSV path.
    #[arg(long, default_value = "retune_matrix.csv")]
    csv: String,
}

/// A channel: plan label, number, and centre frequency in Hz.
struct Chan {
    label: String,
    hz: f64,
}

fn channels() -> Vec<Chan> {
    let mut v = Vec::new();
    // 802.15.4 2.4 GHz: 16 channels, 5 MHz spacing.
    for ch in 11..=26 {
        v.push(Chan {
            label: format!("Z{ch}"),
            hz: (2405.0 + 5.0 * (ch as f64 - 11.0)) * 1e6,
        });
    }
    // 802.11ah US, 1 MHz plan: 26 channels across 902–928 MHz.
    for ch in 1..=26 {
        v.push(Chan {
            label: format!("H{ch}"),
            hz: (902.5 + (ch as f64 - 1.0)) * 1e6,
        });
    }
    v
}

fn main() -> Result<()> {
    let args = Args::parse();
    let chans = channels();
    let n = chans.len();

    let dev = Device::from_args(args.device.as_str())
        .with_context(|| format!("cannot open seify device '{}'", args.device))?;
    dev.set_sample_rate(Direction::Rx, 0, args.sample_rate)?;
    println!(
        "device open, {} channels ({} pairs), sample rate {:.3} MSps, stream={}",
        n,
        n * n,
        args.sample_rate / 1e6,
        args.stream
    );

    // Optional RX stream, to reproduce the contention a live receiver creates.
    // Kept in a thread so `read()` is genuinely concurrent with the retunes.
    let streaming = args.stream;
    let stream_dev = dev.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_rx = stop.clone();
    let reader = streaming.then(|| {
        thread::spawn(move || -> Result<u64> {
            let mut rx = stream_dev.rx_streamer(&[0])?;
            rx.activate()?;
            let mut buf = vec![Complex32::new(0.0, 0.0); 8192];
            let mut reads = 0u64;
            while !stop_rx.load(std::sync::atomic::Ordering::Relaxed) {
                if rx.read(&mut [&mut buf[..]], 200_000).is_ok() {
                    reads += 1;
                }
            }
            rx.deactivate()?;
            Ok(reads)
        })
    });

    let mut csv = File::create(&args.csv)?;
    writeln!(csv, "from_label,to_label,from_mhz,to_mhz,ms")?;

    let settle = Duration::from_millis(args.settle_ms);
    let t_start = Instant::now();
    for (i, from) in chans.iter().enumerate() {
        for to in chans.iter() {
            let mut samples = Vec::with_capacity(args.repeat);
            for _ in 0..args.repeat {
                // Park on `from` and let it settle, so the timed call always
                // starts from the same known state.
                dev.set_frequency(Direction::Rx, 0, from.hz)?;
                thread::sleep(settle);

                let t = Instant::now();
                dev.set_frequency(Direction::Rx, 0, to.hz)?;
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let ms = samples[samples.len() / 2];
            writeln!(
                csv,
                "{},{},{:.3},{:.3},{ms:.3}",
                from.label,
                to.label,
                from.hz / 1e6,
                to.hz / 1e6
            )?;
        }
        csv.flush().ok();
        println!(
            "  row {:>3}/{n} {:>4} done  ({:.1} s elapsed)",
            i + 1,
            from.label,
            t_start.elapsed().as_secs_f64()
        );
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(h) = reader {
        match h.join() {
            Ok(Ok(reads)) => println!("stream thread: {reads} reads"),
            Ok(Err(e)) => eprintln!("stream thread failed: {e}"),
            Err(_) => eprintln!("stream thread panicked"),
        }
    }
    println!("wrote {} ({:.1} s total)", args.csv, t_start.elapsed().as_secs_f64());
    Ok(())
}
