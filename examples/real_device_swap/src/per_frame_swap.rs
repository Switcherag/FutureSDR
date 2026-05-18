// per_frame_swap — at every received frame, swap to the other PHY.
//
// Companion bin to `real_device_swap` (timer-driven swap). Here the swap is
// event-driven: each MAC frame surfaced on a `[[controller_taps]]` port
// triggers an immediate swap to the *other* PHY. A "network" CSV sink in the
// controller bridges both decoders and writes the per-frame TX control data
// (step, run, tag, wait_ms, ts_us) plus local rx + swap latencies, so the lowest
// inter-frame switch time the receiver can sustain can be plotted offline.
//
// TX side (separate device) is expected to alternate:
//   even step → 802.15.4 ch 11 (Zigbee)
//   odd  step → 802.11ah probe-request (HaLow S1G ch 6)
// with a per-step inter-frame wait that decreases by 1 ms every step from
// 500 ms down to 0 ms (501 steps total). Each frame carries a 17-byte
// payload: step (u32 LE) | run (u16 LE) | tag (b'Z'/b'H') | wait_ms (u16 LE)
// | ts_us (u64 LE).
//
// Output: per_frame_swap.csv  with one row per received frame.
// Run from this directory so the relative TOML paths resolve:
//   cd examples/real_device_swap && ../../target/release/per_frame_swap

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use futuresdr::runtime::Pmt;
use plugin_api::{FlowgraphController, default_plugin_dir};

const FLOW_ZIGBEE: &str = "flows/zigbee_rx.toml";
const FLOW_HALOW: &str = "flows/wlan_ah_rx_v2.toml";
const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/network_tail.toml";
const CSV_PATH: &str = "per_frame_swap.csv";

/// If no tap frame arrives within this time after the last swap, swap anyway.
/// Bounded so a missed PHY does not stall the sweep forever.
const RX_TIMEOUT: Duration = Duration::from_millis(800);

/// Search a decoded MAC blob for the control payload and parse it.
///
/// Current layout:
/// `step:u32_le | run:u16_le | tag:u8 (b'Z'|b'H') | wait_ms:u16_le | ts_us:u64_le`.
/// Older captures omitted `run`; keep a fallback parser so mixed plugin builds
/// still produce usable output during the transition.
fn parse_payload(blob: &[u8]) -> Option<(u32, Option<u16>, u8, u16, u64)> {
    if blob.len() >= 17 {
        for i in 0..=(blob.len() - 17) {
            let step = u32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]);
            if step > 600 {
                continue;
            }
            let run = u16::from_le_bytes([blob[i + 4], blob[i + 5]]);
            let tag = blob[i + 6];
            if tag != b'Z' && tag != b'H' {
                continue;
            }
            let wait = u16::from_le_bytes([blob[i + 7], blob[i + 8]]);
            if wait > 600 {
                continue;
            }
            let ts = u64::from_le_bytes([
                blob[i + 9],
                blob[i + 10],
                blob[i + 11],
                blob[i + 12],
                blob[i + 13],
                blob[i + 14],
                blob[i + 15],
                blob[i + 16],
            ]);
            return Some((step, Some(run), tag, wait, ts));
        }
    }

    if blob.len() < 15 {
        return None;
    }

    for i in 0..=(blob.len() - 15) {
        let step = u32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]);
        if step > 600 {
            continue;
        }
        let tag = blob[i + 4];
        if tag != b'Z' && tag != b'H' {
            continue;
        }
        let wait = u16::from_le_bytes([blob[i + 5], blob[i + 6]]);
        if wait > 600 {
            continue;
        }
        let ts = u64::from_le_bytes([
            blob[i + 7],
            blob[i + 8],
            blob[i + 9],
            blob[i + 10],
            blob[i + 11],
            blob[i + 12],
            blob[i + 13],
            blob[i + 14],
        ]);
        return Some((step, None, tag, wait, ts));
    }
    None
}

fn other(curr: &str) -> &'static str {
    if curr == FLOW_ZIGBEE {
        FLOW_HALOW
    } else {
        FLOW_ZIGBEE
    }
}

fn phy_name(toml: &str) -> &'static str {
    if toml == FLOW_ZIGBEE { "Z" } else { "H" }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    println!("=== per_frame_swap — swap PHY on every received frame ===");
    println!("Initial PHY: zigbee_rx; alternating with wlan_ah_rx_v2");
    println!("CSV → {CSV_PATH}\n");

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_permanent(TAIL_FLOW)
        .add_swappable(FLOW_ZIGBEE)
        .tap_channel(256);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Same startup dance as real_device_swap: head first, then activate
        // selectors, then start the swappable.
        for &(idx, ref path, perm) in &entries {
            if perm {
                println!("Starting head fg/{idx}/ from '{path}'");
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                println!("Starting swappable fg/{idx}/ from '{path}'");
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        let swap_target = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("at least one swappable flowgraph");

        let mut csv = File::create(CSV_PATH)?;
        writeln!(
            csv,
            "rx_idx,phy_active,frame_event,step,run,tag,wait_ms,ts_us,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut current = FLOW_ZIGBEE;
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms: f64 = 0.0;
        let mut swap_count: u64 = 0;
        let t0 = Instant::now();

        println!(
            "\nReceiver running. Listening for frames. Frame event triggers immediate PHY swap."
        );
        println!("Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            // Wait for one tap (a decoded frame on the current PHY) or time out.
            let mut deadline = FutureExt::fuse(Timer::after(RX_TIMEOUT));
            let frame_event;
            let mut payload_row: Option<(i64, Option<i64>, char, i64, i128)> = None;

            select! {
                _ = deadline => {
                    frame_event = "timeout";
                    println!(
                        "[t={:.1}ms] [timeout {:?}] no frame on {} — swapping anyway",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        RX_TIMEOUT,
                        phy_name(current),
                    );
                }
                msg = tap_rx.next().fuse() => match msg {
                    Some((tap_name, pmt)) => {
                        frame_event = "rx";
                        let blob = match pmt {
                            Pmt::Blob(b) => b,
                            other => {
                                println!("[tap {tap_name}] non-blob pmt: {other:?}");
                                continue;
                            }
                        };
                        match parse_payload(&blob) {
                            Some((step, run, tag, wait, ts)) => {
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B] CSV,{},{},{},{},{}",
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    phy_name(current),
                                    blob.len(),
                                    step,
                                    run.map_or_else(|| "-".to_string(), |value| value.to_string()),
                                    tag as char,
                                    wait,
                                    ts,
                                );
                                payload_row = Some((
                                    step as i64,
                                    run.map(i64::from),
                                    tag as char,
                                    wait as i64,
                                    ts as i128,
                                ));
                            }
                            None => {
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B] no control-payload signature",
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    phy_name(current),
                                    blob.len(),
                                );
                            }
                        }
                    }
                    None => break,
                },
            }

            let rx_t_ms = t0.elapsed().as_secs_f64() * 1000.0;

            // Swap to the other PHY immediately.
            let next = other(current);
            let t_swap = Instant::now();
            if let Err(e) = ctrl.swap(swap_target, next, &rt_handle).await {
                eprintln!("[swap] failed: {e}");
                continue;
            }
            let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
            swap_total_ms += swap_ms;
            swap_count += 1;
            let phy_was = current;
            current = next;

            let (step_s, run_s, tag_s, wait_s, ts_s) = match payload_row {
                Some((s, r, t, w, ts)) => (
                    s.to_string(),
                    r.map_or_else(|| "-1".into(), |value| value.to_string()),
                    t.to_string(),
                    w.to_string(),
                    ts.to_string(),
                ),
                None => (
                    "-1".into(),
                    "-1".into(),
                    "?".into(),
                    "-1".into(),
                    "-1".into(),
                ),
            };

            writeln!(
                csv,
                "{rx_idx},{phy_was_str},{frame_event},{step_s},{run_s},{tag_s},{wait_s},{ts_s},{rx_t_ms:.3},{swap_ms:.3}",
                phy_was_str = phy_name(phy_was),
            )?;
            csv.flush().ok();
            rx_idx += 1;

            if swap_count > 0 && swap_count % 20 == 0 {
                println!(
                    "[stats] {} swaps, avg swap latency = {:.2} ms",
                    swap_count,
                    swap_total_ms / swap_count as f64
                );
            }
        }

        if swap_count > 0 {
            println!(
                "\nFinal: {} swaps, avg swap latency = {:.2} ms",
                swap_count,
                swap_total_ms / swap_count as f64
            );
        }
        Ok(())
    })
}
