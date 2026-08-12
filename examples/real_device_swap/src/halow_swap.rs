// halow_swap — at every received frame, swap between two 802.11ah receivers.
//
// Sibling of `zigbee_swap`, built to the same shape so the two are comparable
// measurement for measurement. Instead of alternating between two Zigbee
// channels it alternates between two HaLow receivers:
//   A: flows/halow_rxA.toml
//   B: flows/halow_rxB.toml
//
// Both flows sit on the same center frequency, so `apply_radio_demand` skips
// the retune ("params already current") and the swap never waits on the
// hardware — the sub-ms path. Give the two flows different `frequency_hz` to
// put the radio back in the loop.
//
// Each decoded frame surfaced on a `[[controller_taps]]` port triggers an
// immediate swap to the other receiver. The same "network" CSV sink in the
// controller records local rx + swap latencies, so the lowest inter-frame
// switch time the receiver can sustain can be plotted offline.
//
// Where zigbee_swap parses the multizig firmware's 19-byte stamp, HaLow frames
// carry no such payload, so each row records the 802.11 sequence number — it
// is what tells you how many frames went by unheard between two receptions.
//
// One substitution against zigbee_swap: the permanent tail is `null_tail.toml`
// rather than `network_tail.toml`. The A/B flows still declare `to = "tail"`
// and are wired identically; the tail just discards instead of pushing to a
// TAP NIC and a log file, neither of which this measurement needs.
//
// Output: halow_swap.csv  with one row per received frame.
// Run from this directory so the relative TOML paths resolve:
//   cd examples/real_device_swap && ../../target/release/halow_swap

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

const FLOW_HALOW_A: &str = "flows/halow_rxA.toml";
const FLOW_HALOW_B: &str = "flows/halow_rxB.toml";
const HEAD_FLOW: &str = "flows/sdr_head.toml";
/// `null_tail.toml`, not zigbee_swap's `network_tail.toml`: the real tail's
/// TapNic needs an `sdrtap0` iface (CAP_NET_ADMIN) and its file sink needs
/// `data/`. With either missing both blocks fail on init, the whole tail FG
/// terminates, and the bridge feeding it backs up until the SeifySource
/// overflows. The null tail keeps the `to = "tail"` wiring and discards.
const TAIL_FLOW: &str = "flows/null_tail.toml";
const CSV_PATH: &str = "halow_swap.csv";

/// If no tap frame arrives within this time after the last swap, swap anyway.
/// Bounded so a missed channel does not stall the sweep forever.
const RX_TIMEOUT: Duration = Duration::from_millis(80000);

/// Parse the 802.11 sequence number out of a decoded MAC frame.
///
/// The tap is on `decoder.rx_frames`, which carries the bare MAC frame (no
/// RFtap header). Sequence control sits at offset 22; the sequence number is
/// its top 12 bits, the fragment number the low 4. Control frames (type 1)
/// have no sequence control and are skipped.
fn parse_seq(frame: &[u8]) -> Option<(u16, u8)> {
    if frame.len() < 24 {
        return None;
    }
    if (frame[0] >> 2) & 0x3 == 1 {
        return None;
    }
    let seq_ctl = u16::from_le_bytes([frame[22], frame[23]]);
    Some((seq_ctl >> 4, (seq_ctl & 0xF) as u8))
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

    println!("=== halow_swap — swap 802.11ah receiver on every received frame ===");
    println!("Initial PHY: halow_rxA; alternating with halow_rxB");
    println!("CSV → {CSV_PATH}\n");

    // Open a second handle to the SDR for fast retunes. The SeifySource block
    // (in the head FG) opens its own handle on the same hardware; for
    // SoapySDR-backed drivers both handles share the underlying device, so
    // set_frequency from either affects the radio. Calling set_frequency on
    // this handle bypasses the flowgraph message system and the SeifySource's
    // work-loop scheduling, dropping retune latency from ms-scale to the
    // hardware floor (~50 µs). With A and B on the same frequency it is never
    // called — the retune is skipped outright.
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
        .add_swappable(FLOW_HALOW_A)
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
            "rx_idx,phy_active,frame_event,seq,frag,len,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut current = FLOW_HALOW_A;
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
            let mut payload_row: Option<(i64, i64, i64)> = None; // (seq, frag, len)

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
                        match parse_seq(&blob) {
                            Some((seq, frag)) => {
                                println!(
                                    "[t={:.1}ms] [rx {} tap={tap_name} {}B] CSV,{},{}",
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    phy_name(current),
                                    blob.len(),
                                    seq,
                                    frag,
                                );
                                payload_row = Some((seq as i64, frag as i64, blob.len() as i64));
                            }
                            None => {
                                // Dump hex on failure so we can see WHY parse failed
                                // (short MAC frame? control frame? garbage decode?).
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

            // Swap to the other HaLow receiver immediately.
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

            let (seq_s, frag_s, len_s) = match payload_row {
                Some((s, f, l)) => (s.to_string(), f.to_string(), l.to_string()),
                None => ("-1".into(), "-1".into(), "-1".into()),
            };

            writeln!(
                csv,
                "{rx_idx},{phy_was_str},{frame_event},{seq_s},{frag_s},{len_s},{rx_t_ms:.3},{swap_ms:.3}",
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
