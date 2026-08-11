// zigbee_swap_short — at every received frame, swap between two Zigbee channels.
//
// Sibling of `zigbee_swap` for the SHORT frame format. Identical machinery —
// same head, same null tail, same swap-on-every-frame policy — the only
// difference is the on-air payload, which carries a 1-byte sequence number and
// the IFS instead of the 19-byte multizig stamp.
//
// PPDU on the air: SHR (4 B preamble + SFD 0xA7) → PHR (1 B length) → PSDU.
// The decoder hands us the PSDU (length byte consumed by the PHR state), so
// offsets below are PSDU-relative:
//
//   off  size  bytes                     field
//   ---  ----  ------------------------  --------------------------------
//     0     2  41 C8                     FCF (LE = 0xC841)
//     2     1  SS                        sequence number, frame & 0xFF
//     3     2  FF FF                     dst PAN = broadcast
//     5     2  FF FF                     dst addr (short) = broadcast
//     7     8  00 00 45 45 42 47 49 5A   src EUI-64 = 00 00 'E''E''B''G''I''Z'
//    15     4  xx xx xx xx               payload = ifs_us (u32 LE)
//    19     2  cc cc                     FCS (CRC-16, computed by the HW)
//
// PSDU length is 21 (0x15) including FCS. As in `zigbee_swap`, the fixed
// EUI-64 is the anchor: the sequence number sits 5 bytes before it and the
// IFS 8 bytes after, so neither offset has to be guessed.
//
// It alternates between two Zigbee receivers:
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
// Output: zigbee_swap_short.csv  with one row per received frame.
// Run from this directory so the relative TOML paths resolve:
//   cd examples/real_device_swap && ../../target/release/zigbee_swap_short

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use futuresdr::runtime::Pmt;
use futuresdr::seify;
use plugin_host::{FlowgraphController, default_plugin_dir};

/// Args used by both the SeifySource block (inside the head FG) and the
/// fast-retune `Device` we open here. Empty string = pick first available
/// device; matches `sdr_head.toml`'s `[[blocks]] id = "sdr"` config.
const SDR_DEVICE_ARGS: &str = "";

const FLOW_ZIGBEE_A: &str = "flows/zigbee_rxA.toml";
const FLOW_ZIGBEE_B: &str = "flows/zigbee_rxB.toml";
const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/null_tail.toml";
const CSV_PATH: &str = "zigbee_swap_short.csv";

/// If no tap frame arrives within this time after the last swap, swap anyway.
/// Bounded so a missed channel does not stall the sweep forever.
const RX_TIMEOUT: Duration = Duration::from_millis(80000);

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
/// Sits at PSDU offset 7-14, between the addressing fields and the payload, so
/// anchoring on it locates both the sequence number and the IFS exactly.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// Parse the short frame: `(seq, ifs_us)`.
///
/// Anchors on the fixed EUI-64 source address, which sits at PSDU offset 7.
/// The sequence number is 5 bytes before it (offset 2) and the IFS is the
/// 4-byte little-endian payload immediately after it (offset 15) — so both are
/// located without assuming the decoder handed us the PSDU at offset 0.
fn parse_payload(blob: &[u8]) -> Option<(u8, u32)> {
    const A: usize = ZIGBEE_EUI64_ANCHOR.len();
    const SEQ_BACK: usize = 5; // anchor at offset 7, sequence number at offset 2
    if blob.len() < SEQ_BACK + A + 4 {
        return None;
    }
    for i in SEQ_BACK..=(blob.len() - A - 4) {
        if blob[i..i + A] != ZIGBEE_EUI64_ANCHOR {
            continue;
        }
        let seq = blob[i - SEQ_BACK];
        let p = i + A;
        let ifs_us = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]);
        return Some((seq, ifs_us));
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

    println!("=== zigbee_swap_short — swap Zigbee channel on every received frame ===");
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
            "rx_idx,phy_active,frame_event,seq,ifs_us,rx_t_ms,swap_ms"
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
            let mut payload_row: Option<(u8, u32)> = None; // (seq, ifs_us)

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
                            Some((seq, ifs_us)) => {
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B] seq={seq} ifs_us={ifs_us}",
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    phy_name(current),
                                    blob.len(),
                                );
                                payload_row = Some((seq, ifs_us));
                            }
                            None => {
                                // Dump hex on failure so we can see WHY parse failed
                                // (anchor missing? truncated PSDU? wrong format?).
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

            let (seq_s, ifs_s) = match payload_row {
                Some((seq, ifs_us)) => (seq.to_string(), ifs_us.to_string()),
                None => ("-1".into(), "-1".into()),
            };

            writeln!(
                csv,
                "{rx_idx},{phy_was_str},{frame_event},{seq_s},{ifs_s},{rx_t_ms:.3},{swap_ms:.3}",
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
