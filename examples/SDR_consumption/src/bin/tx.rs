//! TX power-consumption sweep.
//!
//! Walks the freq × bw × signal × gain grid (2 × 4 × 3 × 4 = 96 configs),
//! transmitting a full-scale test signal for `--dwell` seconds per config.
//!
//! Each config gets a **freshly built flowgraph** (`TxSignalSource → SeifySink`):
//! the device is opened and fully configured via the `Builder`, streamed for the
//! dwell, then torn down. We deliberately do *not* reconfigure freq/rate/gain on
//! a running stream — the seify sink applies those synchronously inside its
//! message handler, which blocks the sink's `work()` during the USB round-trip
//! and underflows tight TX backends (bladeRF: `wait_for_buffer` 2 s timeout →
//! dead stream). Rebuilding keeps every config a clean, correctly-configured
//! stream on every device.
//!
//! Timing runs on a fixed wall-clock grid: each config's teardown+rebuild is
//! absorbed before an absolute deadline `t0 + i·dwell`, so reconfiguration cost
//! never drifts the schedule. Window start/end times are logged to CSV for
//! alignment with an external power meter.

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::block_on;
use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;
use sdr_consumption::{BWS_HZ, DWELL_SECS, FREQS_HZ, FULL_SCALE, GAINS_DB, Signal, TxSignalSource};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "SDR TX power-consumption sweep (freq × bw × signal × gain)")]
struct Args {
    /// Seify device args (e.g. "driver=bladerf"; "" = auto-detect)
    #[clap(short, long, default_value = "")]
    args: String,
    /// TX channel index
    #[clap(short, long, default_value_t = 0)]
    channel: usize,
    /// Dwell time per configuration (seconds)
    #[clap(short, long, default_value_t = DWELL_SECS)]
    dwell: f64,
    /// Output amplitude (full scale = 1.0)
    #[clap(long, default_value_t = FULL_SCALE)]
    amplitude: f32,
    /// CSV schedule output path
    #[clap(short, long, default_value = "sdr_consumption_tx.csv")]
    out: String,
}

fn build(args: &Args, f: f64, bw: f64, sig: Signal, g: f64) -> Result<Flowgraph> {
    let mut fg = Flowgraph::new();
    let src: TxSignalSource = TxSignalSource::new(sig, bw, args.amplitude);
    let src = fg.add_block(src);
    let src_id: BlockId = (&src).into();
    let snk = Builder::new(&args.args)?
        .channel(args.channel)
        .frequency(f)
        .sample_rate(bw)
        .gain(g)
        .build_sink()?;
    let snk = fg.add_block(snk);
    let snk_id: BlockId = (&snk).into();
    fg.connect_dyn(src_id, "output", snk_id, "inputs[0]")?;
    Ok(fg)
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();

    // freq (outer) × bw × signal × gain (inner)
    let mut configs: Vec<(f64, f64, Signal, f64)> = Vec::new();
    for &f in &FREQS_HZ {
        for &bw in &BWS_HZ {
            for &sig in &Signal::ALL {
                for &g in &GAINS_DB {
                    configs.push((f, bw, sig, g));
                }
            }
        }
    }

    println!("TX consumption sweep: {} configs × {:.1}s = {:.0}s total",
        configs.len(), args.dwell, configs.len() as f64 * args.dwell);
    println!("Device args: {:?}  amplitude: {}\n", args.args, args.amplitude);

    let rt = Runtime::new();
    let t0 = Instant::now();
    let mut rows = vec![String::from("index,t_start_s,t_end_s,freq_hz,bw_hz,gain_db,signal")];

    for (i, &(f, bw, sig, g)) in configs.iter().enumerate() {
        let t_start = t0.elapsed().as_secs_f64();
        let deadline = t0 + Duration::from_secs_f64((i as f64 + 1.0) * args.dwell);
        println!("[{i:>2}/{}] f={:>4.0}MHz bw={:>2.0}MHz sig={:<8} gain={:>2.0}dB  (t+{t_start:.2}s)",
            configs.len(), f / 1e6, bw / 1e6, sig.label(), g);

        // Build + start, with one retry to ride out a slow device release.
        let mut started = None;
        for attempt in 0..2 {
            match build(&args, f, bw, sig, g).and_then(|fg| Ok(rt.start_sync(fg)?)) {
                Ok(x) => {
                    started = Some(x);
                    break;
                }
                Err(e) => {
                    eprintln!("[{i}] open/start failed (attempt {}): {e}", attempt + 1);
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }

        if let Some((task, mut handle)) = started {
            sleep_until(deadline);
            let _ = block_on(handle.terminate_and_wait());
            let _ = block_on(task); // drop flowgraph -> close device before reopen
        } else {
            eprintln!("[{i}] skipped (device unavailable)");
            sleep_until(deadline);
        }

        let t_end = t0.elapsed().as_secs_f64();
        rows.push(format!("{i},{t_start:.4},{t_end:.4},{f},{bw},{g},{}", sig.label()));
    }

    std::fs::write(&args.out, rows.join("\n") + "\n")?;
    println!("\nSchedule written to {}", args.out);
    Ok(())
}
