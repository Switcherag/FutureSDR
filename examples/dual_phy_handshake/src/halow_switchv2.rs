// halow_switchv2 — at every received frame, swap between two 802.11ah flows.
//
// Port of `real_device_swap`'s `zigbee_swap` to the HaLow listen PHYs, kept
// structurally identical so the two are comparable measurement for measurement:
//   A: flows/halow_listenA.toml
//   B: flows/halow_listenB.toml
//
// What it takes from zigbee_swap, and why each matters here:
//   - a SECOND seify device handle wired in as the controller's fast freq/gain
//     setter, so a swap's retune bypasses the flowgraph message system and the
//     SeifySource work loop (~50 µs hardware floor instead of ms-scale);
//   - `select!` on the tap channel against an RX_TIMEOUT, so a silent channel
//     swaps anyway instead of stalling;
//   - a deeper tap channel (256), so a burst of decodes cannot backpressure
//     the decoder while the controller is mid-swap;
//   - `swap_ms` per row plus a running average every 20 swaps.
//
// Not carried over: zigbee_swap's `network_tail.toml` permanent tail FG. It
// existed to bridge both decoders into a CSV sink; here the controller writes
// the CSV directly, so a tail would add a flowgraph without adding data.
//
// Instead of the multizig firmware's 19-byte stamp, each row carries the
// 802.11 sequence number lifted from the frame — same job, it is what tells
// you how many frames went by unheard between two receptions.
//
// Output: halow_switchv2.csv, one row per received frame (and per timeout).
// The columns are a superset of halow_switch.csv's, so `plot_halow_switch.py`
// reads this file unchanged.
//
// Run from this directory so the relative TOML paths resolve:
//   cd examples/dual_phy_handshake && ../../target/release/halow_switchv2

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use futuresdr::runtime::Pmt;
use futuresdr::seify;
use plugin_host::{FlowgraphController, default_plugin_dir};

#[derive(Parser)]
#[command(about = "Swap between two 802.11ah flows on every decoded frame, logging RFTAP to CSV.")]
struct Args {
    /// Stay on the starting flow instead of alternating A ↔ B. The control for
    /// the swap's cost: same PHY, same radio, same logging, no `ctrl.swap()` —
    /// so what a swap costs is the difference between the two runs. Rows still
    /// land in the CSV, with `swap_ms` at 0 and `flow` never changing.
    #[arg(long)]
    no_swap: bool,
}

/// Args for the fast-retune `Device`. Empty string = first available device;
/// matches `sdr_head_listen.toml`'s `[[blocks]] id = "sdr"` config, so both
/// handles land on the same hardware.
const SDR_DEVICE_ARGS: &str = "";

const FLOW_HALOW_A: &str = "flows/halow_listenA.toml";
const FLOW_HALOW_B: &str = "flows/halow_listenB.toml";
const HEAD_FLOW: &str = "flows/sdr_head_listen.toml";
const CSV_PATH: &str = "halow_switchv2.csv";

/// If no tap frame arrives within this time after the last swap, swap anyway.
/// Bounded so a dead channel does not stall the run forever.
const RX_TIMEOUT: Duration = Duration::from_millis(80000);

/// Pull the 802.11 sequence number out of an RFTAP blob.
///
/// RFTAP header: magic `RFta`, u16 length in 32-bit words, u16 present-flags,
/// then (bit 0 set) the u32 DLT. The encapsulated frame follows; sequence
/// control sits at MAC offset 22, and the sequence number is its top 12 bits.
fn parse_seq(blob: &[u8]) -> Option<u16> {
    if blob.len() < 12 || &blob[0..4] != b"RFta" {
        return None;
    }
    let header_len = u16::from_le_bytes([blob[4], blob[5]]) as usize * 4;
    if header_len < 12 || blob.len() < header_len + 24 {
        return None;
    }
    let frame = &blob[header_len..];
    if (frame[0] >> 2) & 0x3 == 1 {
        return None; // control frames carry no sequence control
    }
    Some(u16::from_le_bytes([frame[22], frame[23]]) >> 4)
}

fn other(curr: &str) -> &'static str {
    if curr == FLOW_HALOW_A {
        FLOW_HALOW_B
    } else {
        FLOW_HALOW_A
    }
}

fn phy_name(toml: &str) -> &'static str {
    if toml == FLOW_HALOW_A { "A" } else { "B" }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();
    let no_swap = args.no_swap;

    println!("=== halow_switchv2 — swap 802.11ah flow on every received frame ===");
    if no_swap {
        println!("--no-swap: staying on halow_listenA, no swaps (baseline)");
    } else {
        println!("Initial PHY: halow_listenA; alternating with halow_listenB");
    }
    println!("CSV → {CSV_PATH}\n");

    // Second handle on the SDR for fast retunes. The SeifySource block (in the
    // head FG) opens its own handle on the same hardware; for SoapySDR-backed
    // drivers both share the underlying device, so set_frequency from either
    // moves the radio. Going through this handle skips the flowgraph message
    // system and the SeifySource's work-loop scheduling.
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
        .add_swappable(FLOW_HALOW_A)
        .tap_channel(256);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Head first, then activate selectors, then the swappable listener.
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

        // Wire the fast-retune device in *after* the head FG is running —
        // calling set_frequency before the SeifySource has activated its
        // streamer can confuse the driver.
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
            "rx_idx,flow,frame_event,seq,rx_epoch_s,elapsed_s,swap_ms,rftap_len,rftap_hex"
        )?;
        csv.flush().ok();

        let mut current = FLOW_HALOW_A;
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms: f64 = 0.0;
        let mut swap_count: u64 = 0;
        let t0 = Instant::now();

        if no_swap {
            println!("\nReceiver running. Logging every decoded frame; never swapping.");
        } else {
            println!("\nReceiver running. Every decoded frame triggers an immediate flow swap.");
        }
        println!("Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            // Wait for one tap (a decoded frame on the current PHY) or time out.
            let mut deadline = FutureExt::fuse(Timer::after(RX_TIMEOUT));
            let frame_event;
            let mut frame_row: Option<(u16, Vec<u8>)> = None; // (seq, rftap blob)

            select! {
                _ = deadline => {
                    frame_event = "timeout";
                    println!(
                        "[t={:.1}ms] [timeout {:?}] no frame on {} — {}",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        RX_TIMEOUT,
                        phy_name(current),
                        if no_swap { "still listening" } else { "swapping anyway" },
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
                        match parse_seq(&blob) {
                            Some(seq) => {
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B] seq={seq}",
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    phy_name(current),
                                    blob.len(),
                                );
                                frame_row = Some((seq, blob));
                            }
                            None => {
                                // Dump hex on failure so we can see WHY the parse
                                // failed (not RFTAP? short MAC frame? control frame?).
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

            let rx_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let rx_t_s = t0.elapsed().as_secs_f64();
            

            // Swap to the other flow immediately — unless --no-swap, where the
            // run stays on the starting flow and swap_ms is logged as 0.
            let phy_was = current;
            let swap_ms = if no_swap {
                0.0
            } else {
                let next = other(current);
                let t_swap = Instant::now();
                if let Err(e) = ctrl.swap(swap_target, next, &rt_handle).await {
                    eprintln!("[swap] failed: {e}");
                    continue;
                }
                let ms = t_swap.elapsed().as_secs_f64() * 1000.0;
                swap_total_ms += ms;
                swap_count += 1;
                current = next;
                ms
            };

            let (seq_s, len_s, hex_s) = match &frame_row {
                Some((seq, blob)) => (
                    seq.to_string(),
                    blob.len().to_string(),
                    blob.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                ),
                None => ("-1".into(), "0".into(), String::new()),
            };

            writeln!(
                csv,
                "{rx_idx},{phy},{frame_event},{seq_s},{}.{:09},{rx_t_s:.6},{swap_ms:.3},{len_s},{hex_s}",
                rx_epoch.as_secs(),
                rx_epoch.subsec_nanos(),
                phy = phy_name(phy_was),
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
