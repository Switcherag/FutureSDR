// zigbee_swap — at every received frame, swap between two Zigbee channels.
//
// Sibling of `per_frame_swap` that swaps PHY at every received frame, but
// instead of alternating Zigbee ↔ HaLow, it alternates between two Zigbee
// receivers on different channels:
//   A: flows/zigbee_rxA.toml  (2.425 GHz — channel 11)
//   B: flows/zigbee_rxB.toml  (2.45  GHz)
//
// Each MAC frame surfaced on a `[[controller_taps]]` port triggers an
// immediate swap to the other channel. A "network" CSV sink in the
// controller bridges both decoders and writes the per-frame TX control
// data (step, run, tag, wait_ms, ts_us) plus local rx + swap latencies,
// so the lowest inter-frame switch time the receiver can sustain can be
// plotted offline.
//
// Output: zigbee_swap.csv  with one row per received frame.
// Run from this directory so the relative TOML paths resolve:
//   cd examples/real_device_swap && ../../target/release/zigbee_swap

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use futuresdr::runtime::Pmt;
use futuresdr::seify;
use plugin_api::{FlowgraphController, default_plugin_dir};

/// Args used by both the SeifySource block (inside the head FG) and the
/// fast-retune `Device` we open here. Empty string = pick first available
/// device; matches `sdr_head.toml`'s `[[blocks]] id = "sdr"` config.
const SDR_DEVICE_ARGS: &str = "";

const FLOW_ZIGBEE_A: &str = "flows/zigbee_rxA.toml";
const FLOW_ZIGBEE_B: &str = "flows/zigbee_rxB.toml";
const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/network_tail.toml";
const CSV_PATH: &str = "zigbee_swap.csv";

/// If no tap frame arrives within this time after the last swap, swap anyway.
/// Bounded so a missed channel does not stall the sweep forever.
const RX_TIMEOUT: Duration = Duration::from_millis(800);

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
/// Sits at MHR offset 7-14, immediately before the 17-byte stamp payload, so
/// anchoring on it gives a deterministic stamp offset without offset guessing.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// Parse the 19-byte stamp emitted by the multizig firmware:
/// `step:u32_le | run:u16_le | tag:u8 ('Z') | wait_us:u32_le | ts_us:u64_le`.
///
/// Locates the stamp by searching for the fixed EUI-64 source address
/// (`ZIGBEE_EUI64_ANCHOR`) — the stamp is the 19 bytes immediately after it.
fn parse_payload(blob: &[u8]) -> Option<(u32, u16, u8, u32, u64)> {
    let needed = ZIGBEE_EUI64_ANCHOR.len() + 19;
    if blob.len() < needed {
        return None;
    }
    let max = blob.len() - needed;
    for i in 0..=max {
        if blob[i..i + ZIGBEE_EUI64_ANCHOR.len()] != ZIGBEE_EUI64_ANCHOR {
            continue;
        }
        let p = i + ZIGBEE_EUI64_ANCHOR.len();
        let step = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]);
        let run = u16::from_le_bytes([blob[p + 4], blob[p + 5]]);
        let tag = blob[p + 6];
        let wait_us = u32::from_le_bytes([blob[p + 7], blob[p + 8], blob[p + 9], blob[p + 10]]);
        let ts = u64::from_le_bytes([
            blob[p + 11],
            blob[p + 12],
            blob[p + 13],
            blob[p + 14],
            blob[p + 15],
            blob[p + 16],
            blob[p + 17],
            blob[p + 18],
        ]);
        return Some((step, run, tag, wait_us, ts));
    }
    None
}

fn other(curr: &str) -> &'static str {
    if curr == FLOW_ZIGBEE_A {
        FLOW_ZIGBEE_B
    } else {
        FLOW_ZIGBEE_A
    }
}

fn phy_name(toml: &str) -> &'static str {
    if toml == FLOW_ZIGBEE_A { "A" } else { "B" }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    println!("=== zigbee_swap — swap Zigbee channel on every received frame ===");
    println!("Initial PHY: zigbee_rxA (2.425 GHz); alternating with zigbee_rxB (2.45 GHz)");
    println!("CSV → {CSV_PATH}\n");

    // Open a second handle to the SDR for fast retunes. The SeifySource
    // block (in the head FG) opens its own handle on the same hardware;
    // for SoapySDR-backed drivers both handles share the underlying
    // device, so set_frequency from either affects the radio. Calling
    // set_frequency on this handle bypasses the flowgraph message system
    // and the SeifySource's work-loop scheduling, dropping retune
    // latency from ms-scale to the hardware floor (~50 µs).
    let fast_retune_dev = match seify::Device::from_args(SDR_DEVICE_ARGS) {
        Ok(dev) => {
            println!("opened second SDR handle for fast retune");
            Some(dev)
        }
        Err(e) => {
            eprintln!(
                "could not open second SDR handle ({e}) — falling back to message-based retune"
            );
            None
        }
    };

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_permanent(TAIL_FLOW)
        .add_swappable(FLOW_ZIGBEE_A)
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

        // Wire the fast-retune device (if we got one) into the controller
        // *after* the head FG is running — calling set_frequency before
        // the SeifySource has activated its streamer can confuse the driver.
        if let Some(dev) = fast_retune_dev {
            let dev_freq = dev.clone();
            ctrl.set_fast_freq_setter(move |freq_hz| {
                if let Err(e) = dev_freq.set_frequency(seify::Direction::Rx, 0, freq_hz) {
                    eprintln!("[fast retune] set_frequency({freq_hz}) failed: {e}");
                }
            });
            ctrl.set_fast_gain_setter(move |gain_db| {
                if let Err(e) = dev.set_gain(seify::Direction::Rx, 0, gain_db) {
                    eprintln!("[fast retune] set_gain({gain_db}) failed: {e}");
                }
            });
        }

        let swap_target = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("at least one swappable flowgraph");

        let mut csv = File::create(CSV_PATH)?;
        writeln!(
            csv,
            "rx_idx,phy_active,frame_event,step,run,tag,wait_us,ts_us,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut current = FLOW_ZIGBEE_A;
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms: f64 = 0.0;
        let mut swap_count: u64 = 0;
        let t0 = Instant::now();

        println!(
            "\nReceiver running. Listening for frames. Frame event triggers immediate channel swap."
        );
        println!("Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            // Wait for one tap (a decoded frame on the current PHY) or time out.
            let mut deadline = FutureExt::fuse(Timer::after(RX_TIMEOUT));
            let frame_event;
            let mut payload_row: Option<(i64, i64, char, i64, i128)> = None; // (step, run, tag, wait_us, ts)

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
                            Some((step, run, tag, wait_us, ts)) => {
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B] CSV,{},{},{},{},{}",
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    phy_name(current),
                                    blob.len(),
                                    step,
                                    run,
                                    tag as char,
                                    wait_us,
                                    ts,
                                );
                                payload_row = Some((
                                    step as i64,
                                    run as i64,
                                    tag as char,
                                    wait_us as i64,
                                    ts as i128,
                                ));
                            }
                            None => {
                                // Dump hex on failure so we can see WHY parse failed
                                // (anchor missing? short MAC frame? ACK / beacon?).
                                let hex: String = blob
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B FAIL] {hex}",
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

            // Swap to the other Zigbee channel immediately.
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
                    r.to_string(),
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
