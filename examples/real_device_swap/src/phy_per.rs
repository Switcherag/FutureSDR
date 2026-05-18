// phy_per — single-PHY receiver that holds one flowgraph across multiple
// RX-gain values, retuning the SDR live between gain steps.
//
// CLI surface (driven by `run_per_sweep.py`):
//
//   --flow <toml>     path to a PHY flow TOML (e.g. flows/zigbee_rx.toml)
//   --gains <list>    comma-separated gain values in dB (e.g. 0,4,8,12,16,20)
//   --out <csv>       output CSV; one row per decoded frame, tagged with
//                     the currently-active gain
//   --max-wait-s <s>  hard wall-clock cap on the whole run (default 1800)
//
// Protocol on stdin (one command per line):
//
//   NEXT     advance to the next gain in `--gains`
//   QUIT     finish the current frame, close CSV, exit cleanly
//
// On startup phy_per applies the *first* gain in the list, prints a
// `READY gain=<g>` line on stdout, then streams frames. After every
// `NEXT` it retunes via `FlowgraphController::set_gain`, then prints
// `READY gain=<g>` again so the orchestrator can synchronise.

use std::fs::File;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futures::channel::mpsc as fmpsc;
use futuresdr::futures::{FutureExt, SinkExt, StreamExt, select};
use futuresdr::runtime::Pmt;
use plugin_api::{FlowgraphController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/network_tail.toml";

#[derive(Parser, Debug, Clone)]
#[command(about = "Single-PHY RX runner with live gain sweep")]
struct Args {
    /// Path to the PHY flow TOML (e.g. flows/zigbee_rx.toml).
    #[arg(long)]
    flow: PathBuf,

    /// Comma-separated list of RX gains in dB to walk through, in order.
    /// First entry is applied at startup; `NEXT` on stdin advances.
    #[arg(long)]
    gains: String,

    /// CSV path. One row per received frame, columns:
    /// `gain_db, rx_idx, t_ms, tap, blob_len`.
    #[arg(long)]
    out: PathBuf,

    /// Hard wall-clock cap on the entire run, in seconds (safety net).
    #[arg(long, default_value_t = 1800.0)]
    max_wait_s: f64,
}

fn parse_gains(s: &str) -> Result<Vec<f64>, Box<dyn std::error::Error + Send + Sync>> {
    let v: Result<Vec<f64>, _> = s.split(',').map(|x| x.trim().parse::<f64>()).collect();
    let v = v.map_err(|e| format!("invalid --gains value '{s}': {e}"))?;
    if v.is_empty() {
        return Err("--gains must be non-empty".into());
    }
    Ok(v)
}

/// Override the swappable flow's `[radio].gain_db` with `initial_gain`
/// so the controller's startup sets that gain before we begin.
fn rewrite_flow_with_gain(
    flow_path: &PathBuf,
    gain: f64,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(flow_path)
        .map_err(|e| format!("cannot read flow '{}': {e}", flow_path.display()))?;
    let mut doc: toml::Value = toml::from_str(&content)
        .map_err(|e| format!("invalid TOML '{}': {e}", flow_path.display()))?;
    let radio = doc
        .as_table_mut()
        .and_then(|t| {
            t.entry("radio")
                .or_insert(toml::Value::Table(toml::value::Table::new()))
                .as_table_mut()
        })
        .ok_or("flow TOML has no [radio] table and could not create one")?;
    radio.insert("gain_db".into(), toml::Value::Float(gain));

    let stem = flow_path.file_stem().and_then(|s| s.to_str()).unwrap_or("flow");
    let tmp = std::env::temp_dir().join(format!(
        "phy_per_{stem}_g{:.0}_{}.toml",
        gain,
        std::process::id()
    ));
    std::fs::write(&tmp, toml::to_string(&doc)?)?;
    Ok(tmp)
}

/// Spawn a blocking thread that reads stdin line-by-line and forwards
/// each line to an async mpsc channel that the runtime can poll.
fn spawn_stdin_reader() -> fmpsc::Receiver<String> {
    let (mut tx, rx) = fmpsc::channel::<String>(16);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break,           // EOF
                Ok(_) => {}
                Err(_) => break,
            }
            let trimmed = line.trim().to_string();
            // try_send on an mpsc::channel is sync; use a tiny block_on.
            if futuresdr::async_io::block_on(tx.send(trimmed)).is_err() {
                break;
            }
        }
    });
    rx
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();

    let gains = parse_gains(&args.gains)?;
    let initial_gain = gains[0];

    println!(
        "phy_per: flow={} gains={:?} out={} max_wait={}s",
        args.flow.display(),
        gains,
        args.out.display(),
        args.max_wait_s
    );

    let tmp_flow = rewrite_flow_with_gain(&args.flow, initial_gain)?;
    let tmp_flow_str = tmp_flow.to_string_lossy().to_string();
    let max_wait = Duration::from_secs_f64(args.max_wait_s);
    let out_path = args.out.clone();
    let tmp_to_clean = tmp_flow.clone();

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_permanent(TAIL_FLOW)
        .add_swappable(&tmp_flow_str)
        .tap_channel(1024);

    let mut cmd_rx = spawn_stdin_reader();

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        for &(idx, ref path, perm) in &entries {
            if perm {
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let mut csv = File::create(&out_path)?;
        writeln!(csv, "gain_db,rx_idx,t_ms,tap,blob_len")?;
        csv.flush().ok();

        let mut gain_idx: usize = 0;
        let mut current_gain: f64 = initial_gain;
        let t0 = Instant::now();
        let mut rx_idx: usize = 0;

        // Tell the orchestrator we're settled on the first gain.
        println!("READY gain={current_gain}");

        let advance = |idx: &mut usize, gain: &mut f64| -> Option<f64> {
            if *idx + 1 >= gains.len() {
                None
            } else {
                *idx += 1;
                *gain = gains[*idx];
                Some(*gain)
            }
        };

        loop {
            let remaining = max_wait.saturating_sub(t0.elapsed());
            if remaining.is_zero() {
                eprintln!("[phy_per] global timeout after {:.1}s", t0.elapsed().as_secs_f64());
                break;
            }
            let mut deadline = FutureExt::fuse(Timer::after(remaining));
            select! {
                _ = deadline => {
                    eprintln!("[phy_per] global timeout"); break;
                }
                cmd = cmd_rx.next().fuse() => match cmd {
                    Some(c) if c.eq_ignore_ascii_case("NEXT") => {
                        match advance(&mut gain_idx, &mut current_gain) {
                            Some(new_gain) => {
                                if let Err(e) = ctrl.set_gain(new_gain).await {
                                    eprintln!("[phy_per] set_gain({new_gain}) failed: {e}");
                                }
                                println!("READY gain={current_gain}");
                            }
                            None => {
                                println!("DONE no_more_gains");
                                break;
                            }
                        }
                    }
                    Some(c) if c.eq_ignore_ascii_case("QUIT") => {
                        println!("DONE quit");
                        break;
                    }
                    Some(other) => {
                        eprintln!("[phy_per] unknown stdin command: {other:?}");
                    }
                    None => {
                        eprintln!("[phy_per] stdin closed");
                        break;
                    }
                },
                msg = tap_rx.next().fuse() => match msg {
                    Some((tap, Pmt::Blob(b))) => {
                        let t_ms = t0.elapsed().as_secs_f64() * 1000.0;
                        writeln!(csv, "{current_gain},{rx_idx},{t_ms:.3},{tap},{}", b.len())?;
                        rx_idx += 1;
                        // Keep stdout chatty enough for the orchestrator
                        // to observe progress without flooding logs.
                        if rx_idx % 100 == 0 {
                            csv.flush().ok();
                        }
                    }
                    Some(_) => {}
                    None => {
                        eprintln!("[phy_per] tap channel closed");
                        break;
                    }
                },
            }
        }

        csv.flush().ok();
        println!(
            "[phy_per] done: total_rx={} gains_walked={} elapsed={:.2}s out={}",
            rx_idx,
            gain_idx + 1,
            t0.elapsed().as_secs_f64(),
            out_path.display()
        );
        let _ = std::fs::remove_file(&tmp_to_clean);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;

    Ok(())
}
