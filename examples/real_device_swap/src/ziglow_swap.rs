// ziglow_swap — at every received frame, swap between two *different* PHYs.
//
// Sibling of `zigbee_swap` / `zigbee_swap_short`: same head, same null tail,
// same swap-on-every-frame policy. The difference is that the two swappable
// flows are not two channels of one PHY but two whole PHYs, so each swap also
// retunes the radio across bands:
//
//   Z: flows/zigbee_rxA.toml   802.15.4 @ 2.425 GHz (channel 15)
//   H: flows/halow_rxA.toml    802.11ah  @ 919 MHz
//
// The two carry very different amounts of information, and the CSV reflects
// that rather than pretending they are symmetric:
//
//   Z frames — the multizig firmware's 19-byte stamp, tag 'Z', located by
//     anchoring on the fixed EUI-64 source address. Gives step, run, wait_us
//     and the transmit timestamp, so Z alone pins the sweep position exactly.
//
//   H frames — RPG with enable_random=1, PSDU = RPG_SIZE (30 B by default).
//     The payload is pseudo-random and carries no application marker; source
//     and destination are the MM6108's MAC and broadcast. Only the length and
//     the timing are meaningful. The 802.11 sequence-control field in the MAC
//     header is also recorded — the RPG does increment it, so it is usable as
//     a frame counter — but nothing in the payload is.
//
// Which PHY decoded a frame is taken from the *tap name*, not from the
// controller's idea of the current flow, so a frame that lands across a swap
// boundary is still attributed correctly.
//
// Output: ziglow_swap.csv, one row per received frame (and per timeout).
// Run from this directory so the relative TOML paths resolve:
//   cd examples/real_device_swap && ../../target/release/ziglow_swap

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

const FLOW_Z: &str = "flows/zigbee_rxA.toml";
const FLOW_H: &str = "flows/halow_rxA.toml";
const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/null_tail.toml";
const CSV_PATH: &str = "ziglow_swap.csv";

/// `[[controller_taps]] name` declared by each flow above. Used to attribute a
/// decoded frame to the PHY that produced it.
const TAP_Z: &str = "zigbee_frames";

/// If no tap frame arrives within this time after the last swap, swap anyway.
/// Bounded so a silent band does not stall the sweep forever.
const RX_TIMEOUT: Duration = Duration::from_millis(80000);

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
/// Sits at MHR offset 7-14, immediately before the 19-byte stamp, so anchoring
/// on it gives a deterministic stamp offset without offset guessing.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// What a decoded frame yielded, per PHY.
enum Frame {
    /// Z: `step, run, tag, wait_us, ts_us` from the firmware stamp.
    Z(u32, u16, u8, u32, u64),
    /// H: the 802.11 sequence number. `None` for a frame too short or of a
    /// type that carries no sequence control.
    H(Option<u16>),
}

/// Parse the 19-byte multizig stamp:
/// `step:u32_le | run:u16_le | tag:u8 ('Z') | wait_us:u32_le | ts_us:u64_le`.
fn parse_zigbee(blob: &[u8]) -> Option<(u32, u16, u8, u32, u64)> {
    let needed = ZIGBEE_EUI64_ANCHOR.len() + 19;
    if blob.len() < needed {
        return None;
    }
    for i in 0..=(blob.len() - needed) {
        if blob[i..i + ZIGBEE_EUI64_ANCHOR.len()] != ZIGBEE_EUI64_ANCHOR {
            continue;
        }
        let p = i + ZIGBEE_EUI64_ANCHOR.len();
        let step = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]);
        let run = u16::from_le_bytes([blob[p + 4], blob[p + 5]]);
        let tag = blob[p + 6];
        let wait_us = u32::from_le_bytes([blob[p + 7], blob[p + 8], blob[p + 9], blob[p + 10]]);
        let ts = u64::from_le_bytes([
            blob[p + 11], blob[p + 12], blob[p + 13], blob[p + 14],
            blob[p + 15], blob[p + 16], blob[p + 17], blob[p + 18],
        ]);
        return Some((step, run, tag, wait_us, ts));
    }
    None
}

/// Pull the 802.11 sequence number out of a decoded HaLow MAC frame.
///
/// The tap is on `decoder.rx_frames`, so this is the bare MAC frame with no
/// RFtap header. Sequence control sits at offset 22; the sequence number is
/// its top 12 bits. Control frames (type 1) carry none.
fn parse_halow_seq(frame: &[u8]) -> Option<u16> {
    if frame.len() < 24 || (frame[0] >> 2) & 0x3 == 1 {
        return None;
    }
    Some(u16::from_le_bytes([frame[22], frame[23]]) >> 4)
}

fn other(curr: &str) -> &'static str {
    if curr == FLOW_Z { FLOW_H } else { FLOW_Z }
}

fn phy_name(toml: &str) -> &'static str {
    if toml == FLOW_Z { "Z" } else { "H" }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    println!("=== ziglow_swap — swap PHY on every received frame ===");
    println!("Z: {FLOW_Z} (802.15.4, 2.425 GHz ch15)");
    println!("H: {FLOW_H} (802.11ah, 919 MHz)");
    println!("CSV → {CSV_PATH}\n");

    // Second handle on the SDR for fast retunes. Unlike the same-band swaps,
    // here every swap really does move the LO across bands, so this path is
    // exercised on every single frame.
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
        .add_swappable(FLOW_Z)
        .tap_channel(256);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        for &(idx, ref path, perm) in &entries {
            if perm {
                println!("Starting permanent fg/{idx}/ from '{path}'");
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
        // `phy` is the PHY that decoded the frame (from the tap name); `len` is
        // the decoded frame length, the one field both PHYs always provide.
        // Stamp columns are Z-only, `seq` is H-only; unavailable fields are -1.
        writeln!(
            csv,
            "rx_idx,phy,tap,frame_event,len,step,run,tag,wait_us,ts_us,seq,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut current = FLOW_Z;
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms: f64 = 0.0;
        let mut swap_count: u64 = 0;
        let (mut n_z, mut n_h) = (0u64, 0u64);
        let t0 = Instant::now();

        println!("\nReceiver running. Every decoded frame triggers an immediate PHY swap.");
        println!("Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            let mut deadline = FutureExt::fuse(Timer::after(RX_TIMEOUT));
            let frame_event;
            let mut row: Option<(&'static str, String, usize, Frame)> = None;

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
                        let t = t0.elapsed().as_secs_f64() * 1000.0;

                        // The tap name says which PHY produced this, even if the
                        // controller has already moved on to the other flow.
                        if tap_name == TAP_Z {
                            match parse_zigbee(&blob) {
                                Some((step, run, tag, wait_us, ts)) => {
                                    n_z += 1;
                                    println!(
                                        "[t={t:.1}ms] [rx Z tap={tap_name} {}B] step={step} run={run} wait_us={wait_us}",
                                        blob.len(),
                                    );
                                    row = Some(("Z", tap_name, blob.len(),
                                                Frame::Z(step, run, tag, wait_us, ts)));
                                }
                                None => {
                                    let hex: String = blob.iter()
                                        .map(|b| format!("{b:02x}"))
                                        .collect::<Vec<_>>().join(" ");
                                    println!(
                                        "[t={t:.1}ms] [rx Z tap={tap_name} {}B FAIL] {hex}",
                                        blob.len(),
                                    );
                                }
                            }
                        } else {
                            // H: random payload, no application marker. Length
                            // and timing are the measurement; the MAC sequence
                            // number comes along for free as a frame counter.
                            n_h += 1;
                            let seq = parse_halow_seq(&blob);
                            println!(
                                "[t={t:.1}ms] [rx H tap={tap_name} {}B] seq={}",
                                blob.len(),
                                seq.map_or_else(|| "-".into(), |s| s.to_string()),
                            );
                            row = Some(("H", tap_name, blob.len(), Frame::H(seq)));
                        }
                    }
                    None => break,
                },
            }

            let rx_t_ms = t0.elapsed().as_secs_f64() * 1000.0;

            // Swap to the other PHY immediately. This one really does retune.
            let next = other(current);
            let t_swap = Instant::now();
            if let Err(e) = ctrl.swap(swap_target, next, &rt_handle).await {
                eprintln!("[swap] failed: {e}");
                continue;
            }
            let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
            swap_total_ms += swap_ms;
            swap_count += 1;
            current = next;

            let (phy, tap, len, step, run, tag, wait, ts, seq) = match &row {
                Some((phy, tap, len, Frame::Z(s, r, g, w, t))) => (
                    *phy, tap.clone(), len.to_string(), s.to_string(), r.to_string(),
                    (*g as char).to_string(), w.to_string(), t.to_string(), "-1".to_string(),
                ),
                Some((phy, tap, len, Frame::H(seq))) => (
                    *phy, tap.clone(), len.to_string(),
                    "-1".into(), "-1".into(), "?".into(), "-1".into(), "-1".into(),
                    seq.map_or_else(|| "-1".into(), |s| s.to_string()),
                ),
                None => (
                    "?", String::new(), "-1".into(), "-1".into(), "-1".into(),
                    "?".into(), "-1".into(), "-1".into(), "-1".into(),
                ),
            };

            writeln!(
                csv,
                "{rx_idx},{phy},{tap},{frame_event},{len},{step},{run},{tag},{wait},{ts},{seq},{rx_t_ms:.3},{swap_ms:.3}",
            )?;
            csv.flush().ok();
            rx_idx += 1;

            if swap_count > 0 && swap_count % 20 == 0 {
                println!(
                    "[stats] {swap_count} swaps, avg swap latency = {:.2} ms, Z={n_z} H={n_h}",
                    swap_total_ms / swap_count as f64,
                );
            }
        }

        if swap_count > 0 {
            println!(
                "\nFinal: {swap_count} swaps, avg swap latency = {:.2} ms, Z={n_z} H={n_h}",
                swap_total_ms / swap_count as f64,
            );
        }
        Ok(())
    })
}
