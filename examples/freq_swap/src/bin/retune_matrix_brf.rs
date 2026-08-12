// retune_matrix_brf — the retune matrix, measured straight through libbladeRF.
//
// Same experiment as `retune_matrix`, same channel plans, same CSV schema, so
// the two can be handed to `plot_retune_matrix.py` together and read on one
// colour scale. The only difference is what sits between this code and the
// hardware: SoapySDR + seify there, the C libbladeRF here.
//
// It answers two questions the SoapySDR version cannot:
//
//   1. How much of the measured retune cost is the abstraction layer? If the
//      matrices agree, Soapy is innocent and the cost is libbladeRF or the
//      hardware. If this one is much faster, the swap path should move off
//      Soapy.
//   2. Does the Host/FPGA tuning mode matter? `bladerf_set_tuning_mode` moves
//      the tuning algorithm onto the FPGA, cutting host round-trips.
//      SoapySDR exposes no way to reach it. `--tuning-mode` picks; run it both
//      ways and diff.
//
// Note what is being timed: how long `bladerf_set_frequency` takes to return.
// That is *not* how long the radio is off the air — the RF settles well before
// the call returns. A call that blocks the streaming thread costs air time; one
// that merely returns slowly on its own thread may cost nothing. `--stream`
// is what separates the two: it runs a concurrent reader and reports how many
// reads completed and how many failed, so a retune that stalls the stream
// shows up as read errors rather than as a bigger number in the matrix.
//
// Output: retune_matrix_brf.csv — from_label,to_label,from_mhz,to_mhz,ms
// Plot:   python3 plot_retune_matrix.py retune_matrix.csv retune_matrix_brf.csv \
//             --labels soapy,libbladerf
//
// Build needs the libbladeRF headers and library:
//   cd examples/freq_swap
//   BLADERF_INCLUDE_PATH=/usr/local/include RUSTFLAGS="-L/usr/local/lib64" \
//       cargo run --release --bin retune_matrix_brf -- --stream

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bladerf::{
    BladeRF, BladeRfAny, Channel, ChannelLayoutRx, ComplexI16, RxChannel, StreamConfig, TuningMode,
};
use clap::{Parser, ValueEnum};

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Tuning {
    /// Tuning algorithm runs on the host — slower, more diagnostics.
    Host,
    /// Tuning algorithm runs on the FPGA — the fast path.
    Fpga,
    /// Leave whatever the device came up with, and report it.
    Default,
}

#[derive(Parser, Debug)]
#[command(about = "Time every Zigbee <-> 802.11ah US retune via libbladeRF directly.")]
struct Args {
    /// Hardware sample rate to configure before sweeping (Hz).
    #[arg(long, default_value_t = 20e6)]
    sample_rate: f64,
    /// Run an RX stream during the sweep, so retunes contend with reads.
    #[arg(long)]
    stream: bool,
    /// Tuning mode to select before sweeping.
    #[arg(long, value_enum, default_value_t = Tuning::Default)]
    tuning_mode: Tuning,
    /// Settle time after moving to the `from` channel, before timing (ms).
    #[arg(long, default_value_t = 20)]
    settle_ms: u64,
    /// Repeats per ordered pair; the median is reported.
    #[arg(long, default_value_t = 1)]
    repeat: usize,
    /// Take every Nth channel from each plan — 1 is the full 42x42 matrix,
    /// 4 gives a ~11x11 preview in a fraction of the time.
    #[arg(long, default_value_t = 1)]
    stride: usize,
    /// Output CSV path.
    #[arg(long, default_value = "retune_matrix_brf.csv")]
    csv: String,
}

/// A channel: plan label and centre frequency in Hz.
struct Chan {
    label: String,
    hz: u64,
}

/// Same two plans as `retune_matrix`, so the matrices line up cell for cell.
fn channels(stride: usize) -> Vec<Chan> {
    let mut v = Vec::new();
    for ch in (11..=26).step_by(stride.max(1)) {
        v.push(Chan {
            label: format!("Z{ch}"),
            hz: ((2405.0 + 5.0 * (ch as f64 - 11.0)) * 1e6) as u64,
        });
    }
    for ch in (1..=26).step_by(stride.max(1)) {
        v.push(Chan {
            label: format!("H{ch}"),
            hz: ((902.5 + (ch as f64 - 1.0)) * 1e6) as u64,
        });
    }
    v
}

fn main() -> Result<()> {
    let args = Args::parse();
    let chans = channels(args.stride);
    let n = chans.len();

    let dev = Arc::new(BladeRfAny::open_first().context("cannot open bladeRF")?);
    println!("serial {}", dev.get_serial().unwrap_or_default());

    let actual = dev
        .set_sample_rate(Channel::Rx0, args.sample_rate as u32)
        .context("set_sample_rate")?;

    // Tuning mode is the whole reason for talking to libbladeRF directly, so
    // report what it actually ended up as rather than what was requested.
    match args.tuning_mode {
        Tuning::Host => dev.set_tuning_mode(TuningMode::Host).context("set Host")?,
        Tuning::Fpga => dev.set_tuning_mode(TuningMode::FPGA).context("set FPGA")?,
        Tuning::Default => {}
    }
    let mode = dev.get_tuning_mode().context("get_tuning_mode")?;

    println!(
        "{} channels ({} pairs), sample rate {:.3} MSps, tuning {mode:?}, stream={}",
        n,
        n * n,
        actual as f64 / 1e6,
        args.stream
    );

    // Optional RX stream, so retunes contend with reads exactly as they do in
    // a live receiver. `read()` takes `&self` and the device is Send + Sync,
    // so this needs no lock — which is itself the thing SoapySDR made awkward.
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let read_errs = Arc::new(AtomicU64::new(0));
    let reader = args.stream.then(|| {
        let (dev, stop, reads, read_errs) =
            (dev.clone(), stop.clone(), reads.clone(), read_errs.clone());
        thread::spawn(move || -> Result<()> {
            let rx = BladeRfAny::rx_streamer_arc::<ComplexI16>(
                dev,
                StreamConfig::default(),
                ChannelLayoutRx::SISO(RxChannel::Rx0),
            )?;
            rx.enable()?;
            let mut buf = vec![ComplexI16::new(0, 0); 8192];
            while !stop.load(Ordering::Relaxed) {
                match rx.read(&mut buf, Duration::from_millis(200)) {
                    Ok(()) => reads.fetch_add(1, Ordering::Relaxed),
                    Err(_) => read_errs.fetch_add(1, Ordering::Relaxed),
                };
            }
            rx.disable()?;
            Ok(())
        })
    });
    if args.stream {
        // Let the stream reach steady state before the first timed call, or
        // the first row measures stream startup instead of retune cost.
        thread::sleep(Duration::from_millis(200));
    }

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
                dev.set_frequency(Channel::Rx0, from.hz)?;
                thread::sleep(settle);

                let t = Instant::now();
                dev.set_frequency(Channel::Rx0, to.hz)?;
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let ms = samples[samples.len() / 2];
            writeln!(
                csv,
                "{},{},{:.3},{:.3},{ms:.3}",
                from.label,
                to.label,
                from.hz as f64 / 1e6,
                to.hz as f64 / 1e6
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

    stop.store(true, Ordering::Relaxed);
    if let Some(h) = reader {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("stream thread failed: {e}"),
            Err(_) => eprintln!("stream thread panicked"),
        }
        // A retune that stalls the stream shows up here, not in the matrix:
        // reads that timed out while libbladeRF was busy reconfiguring.
        println!(
            "stream: {} reads ok, {} failed",
            reads.load(Ordering::Relaxed),
            read_errs.load(Ordering::Relaxed)
        );
    }
    println!(
        "wrote {} ({:.1} s total, tuning {mode:?})",
        args.csv,
        t_start.elapsed().as_secs_f64()
    );
    Ok(())
}
