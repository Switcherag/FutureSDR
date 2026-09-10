// ziglow_replay — the ziglow per-frame PHY swap, driven by a recorded IQ file.
//
// Same experiment and the SAME TOPOLOGY as `ziglow_swap` / `halow_swap`:
//
//     flows/samples_head.toml ─► <PHY flow, swapped per frame> ─► null_tail
//
//   Z: flows/zigbee_rxA.toml   802.15.4
//   H: flows/halowv6A.toml     802.11ah
//
// The only thing replaced is the head. Where `ziglow_swap` does
// `add_head(sdr_head.toml)` and gets SeifySource, this does
// `add_head(samples_head.toml)` and gets FileSourceC32 → Throttle(4 MSps).
// Everything downstream — the bridge out of the head, its `connected` gate,
// how the controller handles that gate across a swap, the tail — is identical,
// which is the point: a capture taken any other way is not comparable with
// `halow_swapv6*.csv`.
//
// An earlier version of this binary fed the swappable directly through
// `connect_radio` (the shape `ziglow_swap_quicktune` uses, because there the
// binary owns the radio). That measured a different thing: it left the head
// flowgraph and its bridge out of the path entirely, so its numbers could not
// be put next to the recorded sweeps.
//
// NO RADIO. `samples_head.toml` declares no `capabilities`, so the `[radio]`
// frequency and gain demands in the PHY flows resolve against nothing. What is
// measured is the software swap alone: tear one flowgraph down, stand the
// other up, and whether that finished before the next frame arrived.
//
// THROTTLE IS WHAT MAKES IT REAL. The head advances the file at 4 MSps of wall
// clock, and keeps advancing across a swap — the bridge sink discards while its
// gate is closed rather than stalling, exactly as a radio keeps streaming into
// a receiver that has stopped listening. Read the file as fast as it comes off
// disk and every swap is free and every PER is zero.
//
// Because the two reference frames are single recordings replayed verbatim,
// every copy is byte-identical: the stamps and sequence numbers inside them
// repeat, so they cannot identify a transmission. PER is counted instead
// against the generator's manifest — `sent_H` / `sent_Z` in the sidecar —
// which is what `recording/bench/run_sweep.py` does.
//
// Output: the same CSV schema as ziglow_swap.csv.
//
//   cd examples/real_device_swap
//   ../../target/release/ziglow_replay --iq recording/bench/iq/ifs_2.000.cf32 \
//       --csv recording/bench/csv/ifs_2.000.csv

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futures::{select, FutureExt, StreamExt};
use futuresdr::runtime::Pmt;
use plugin_host::{default_plugin_dir, FlowgraphController};

const HEAD_FLOW: &str = "flows/samples_head.toml";
const FLOW_Z: &str = "flows/zigbee_rxA.toml";
const FLOW_H: &str = "flows/halowv6A.toml";
const TAIL_FLOW: &str = "flows/null_tail.toml";

/// `[[controller_taps]] name` declared by the Zigbee flow. Which PHY decoded a
/// frame is taken from the tap name, not from the controller's current flow,
/// so a frame landing across a swap boundary is still attributed correctly.
const TAP_Z: &str = "zigbee_frames";

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// What a decoded frame carries, which differs sharply by PHY.
enum Frame {
    Z(u32, u16, u8, u32, u64),
    H(Option<u16>),
    Unstamped,
}

#[derive(Parser, Debug)]
#[command(about = "Replay a recorded alternating-PHY IQ file through the per-frame swap, with no radio.")]
struct Args {
    /// Input cf32 file from recording/bench/gen_iq.py.
    #[arg(long)]
    iq: PathBuf,

    /// Output CSV path (ziglow schema).
    #[arg(long)]
    csv: PathBuf,

    /// Head flow TOML. Its FileSourceC32 path is rewritten to `--iq`.
    #[arg(long, default_value = HEAD_FLOW)]
    head: PathBuf,

    /// Sample rate the file was built at. Used to work out how long the replay
    /// should take; the head's own Throttle is what actually sets the pace.
    #[arg(long, default_value_t = 4e6)]
    sample_rate: f64,

    /// PHY the file's first frame belongs to (`start_phy` in its sidecar).
    #[arg(long, default_value = "H")]
    start_phy: String,

    /// The pair of flows the receiver alternates between. Defaults to the
    /// cross-PHY pair, which is the experiment this binary is named for.
    ///
    /// Set both to the A and B side of one PHY for a same-PHY sweep — the
    /// swap `zigbee_swap` and `halow_swap` perform, and what the recorded
    /// zigbee_swapfinal.csv and halow_swapv6*.csv time on real hardware:
    ///
    ///   --flow-a flows/zigbee_rxA.toml --flow-b flows/zigbee_rxB.toml
    ///   --flow-a flows/halowv6A.toml   --flow-b flows/halowv6B.toml
    ///
    /// The IQ file must carry only that PHY's frames (`gen_iq.py --pattern Z`
    /// or `--pattern H`), or the flow will be deaf to half of them.
    #[arg(long, default_value = FLOW_Z)]
    flow_a: PathBuf,

    #[arg(long, default_value = FLOW_H)]
    flow_b: PathBuf,

    /// Seconds to keep listening past the file's own duration, so frames still
    /// in the pipeline when the samples run out are not cut off.
    #[arg(long, default_value_t = 2.0)]
    grace: f64,

    /// Do not swap at all: stand up one flow and leave it. The control for
    /// every PER figure this binary produces — whatever it loses with this set
    /// is lost to the stimulus or the receiver, not to swapping, and is the
    /// floor no swap policy can beat.
    #[arg(long)]
    no_swap: bool,

    /// Print the controller's per-swap timing breakdown. Off by default: at a
    /// thousand swaps per file it costs more than the swaps it reports.
    #[arg(long)]
    verbose: bool,
}

fn strip_rftap(blob: &[u8]) -> &[u8] {
    if blob.len() < 8 || &blob[0..4] != b"RFta" {
        return blob;
    }
    let header_len = u16::from_le_bytes([blob[4], blob[5]]) as usize * 4;
    if header_len < 8 || header_len > blob.len() {
        return blob;
    }
    &blob[header_len..]
}

/// Parse the 19-byte stamp the multizig transmitter puts in the payload,
/// located by the fixed EUI-64 source address that precedes it. Identical to
/// `ziglow_swap_quicktune.rs` so the CSV columns mean the same thing.
fn parse_payload(blob: &[u8]) -> Option<(u32, u16, u8, u32, u64)> {
    let frame = strip_rftap(blob);
    let needed = ZIGBEE_EUI64_ANCHOR.len() + 19;
    if frame.len() < needed {
        return None;
    }
    for i in 0..=(frame.len() - needed) {
        if frame[i..i + ZIGBEE_EUI64_ANCHOR.len()] != ZIGBEE_EUI64_ANCHOR {
            continue;
        }
        let p = i + ZIGBEE_EUI64_ANCHOR.len();
        return Some((
            u32::from_le_bytes([frame[p], frame[p + 1], frame[p + 2], frame[p + 3]]),
            u16::from_le_bytes([frame[p + 4], frame[p + 5]]),
            frame[p + 6],
            u32::from_le_bytes([frame[p + 7], frame[p + 8], frame[p + 9], frame[p + 10]]),
            u64::from_le_bytes([
                frame[p + 11], frame[p + 12], frame[p + 13], frame[p + 14],
                frame[p + 15], frame[p + 16], frame[p + 17], frame[p + 18],
            ]),
        ));
    }
    None
}

/// 802.11 sequence control at offset 22; sequence number is its top 12 bits.
fn parse_halow_seq(frame: &[u8]) -> Option<u16> {
    if frame.len() < 24 || (frame[0] >> 2) & 0x3 == 1 {
        return None;
    }
    Some(u16::from_le_bytes([frame[22], frame[23]]) >> 4)
}

/// Write a copy of the head TOML whose FileSourceC32 path is `iq`.
///
/// The head is otherwise a fixed, checked-in declaration; only the file it
/// reads changes from one IFS step to the next. Rewriting a copy rather than
/// the original keeps `flows/samples_head.toml` a stable description of the
/// topology instead of a scratch file that every run edits in place.
fn head_for(template: &PathBuf, iq: &PathBuf) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let src = std::fs::read_to_string(template)?;
    let mut out = String::with_capacity(src.len());
    let mut patched = false;
    for line in src.lines() {
        if !patched && line.trim_start().starts_with("config = [") && line.contains(".cf32") {
            out.push_str(&format!("config = [{:?}, false]\n", iq.to_string_lossy()));
            patched = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !patched {
        return Err(format!(
            "{}: no `config = [\"....cf32\", ...]` line to point at the IQ file",
            template.display()
        )
        .into());
    }
    // Beside the IQ file, so a step's head and its samples travel together and
    // two concurrent runs cannot overwrite each other's head.
    let path = iq.with_extension("head.toml");
    std::fs::write(&path, out)?;
    Ok(path)
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();

    let flow_a: String = args.flow_a.to_string_lossy().into_owned();
    let flow_b: String = args.flow_b.to_string_lossy().into_owned();
    if flow_a == flow_b {
        return Err("--flow-a and --flow-b must differ; a swap needs two flows".into());
    }
    // Which of the pair to start on. With the default cross-PHY pair this is
    // the PHY of the file's first frame; for a same-PHY pair both sides decode
    // it, so it only decides which of A/B goes first.
    let start_flow: String = match args.start_phy.to_ascii_uppercase().as_str() {
        "H" => flow_b.clone(),
        "Z" => flow_a.clone(),
        s => return Err(format!("--start-phy must be H or Z, got {s:?}").into()),
    };

    let n_samples = std::fs::metadata(&args.iq)?.len() / 8; // cf32: 8 bytes/sample
    let file_secs = n_samples as f64 / args.sample_rate;
    let head_path = head_for(&args.head, &args.iq)?;

    println!(
        "replaying {} — {} samples, {:.3} s @ {:.3} MSps, starting on {}",
        args.iq.display(),
        n_samples,
        file_secs,
        args.sample_rate / 1e6,
        if start_flow == flow_a { "A" } else { "B" },
    );
    println!("flows: A={flow_a}  B={flow_b}");
    println!("head: {} (from {})", head_path.display(), args.head.display());

    let head_str = head_path.to_string_lossy().into_owned();
    let csv_path = args.csv.clone();
    let verbose = args.verbose;
    let no_swap = args.no_swap;
    // The head stops producing when the file ends; give the pipeline a moment
    // past that so a frame still in flight is not cut off, then stop.
    let deadline = Duration::from_secs_f64(file_secs + args.grace);

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(head_str)
        .add_permanent(TAIL_FLOW)
        .add_swappable(start_flow.clone())
        .tap_channel(256);
    let (fa, fb) = (flow_a.clone(), flow_b.clone());

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
        ctrl.set_swap_verbose(verbose);

        let swap_target = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("at least one swappable flowgraph");

        if let Some(parent) = csv_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut csv = File::create(&csv_path)?;
        writeln!(
            csv,
            "rx_idx,phy,tap,frame_event,len,step,run,tag,wait_us,ts_us,seq,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut current: String = start_flow.clone();
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms = 0.0f64;
        let mut swap_count: u64 = 0;
        let (mut n_z, mut n_h) = (0u64, 0u64);
        let t0 = Instant::now();

        loop {
            let left = deadline.saturating_sub(t0.elapsed());
            if left.is_zero() {
                break;
            }
            // Wake at least every 200 ms so the deadline is honoured even
            // while no frames are arriving at all.
            let mut tick = FutureExt::fuse(Timer::after(left.min(Duration::from_millis(200))));
            let frame_event;
            let mut row: Option<(&'static str, String, usize, Frame)> = None;

            select! {
                _ = tick => continue,
                msg = tap_rx.next().fuse() => match msg {
                    Some((tap_name, pmt)) => {
                        frame_event = "rx";
                        let blob = match pmt {
                            Pmt::Blob(b) => b,
                            _ => continue,
                        };
                        if tap_name == TAP_Z {
                            n_z += 1;
                            let frame = match parse_payload(&blob) {
                                Some((step, run, tag, wait_us, ts)) => {
                                    Frame::Z(step, run, tag, wait_us, ts)
                                }
                                None => Frame::Unstamped,
                            };
                            row = Some(("Z", tap_name, blob.len(), frame));
                        } else {
                            n_h += 1;
                            row = Some(("H", tap_name, blob.len(),
                                        Frame::H(parse_halow_seq(&blob))));
                        }
                    }
                    None => break,
                },
            }

            let rx_t_ms = t0.elapsed().as_secs_f64() * 1000.0;

            // Swap to the other PHY. Software only — the head declares no
            // capabilities, so the `[radio]` demands resolve against nothing.
            let mut swap_ms = 0.0f64;
            if !no_swap {
                let next: String = if current == fa { fb.clone() } else { fa.clone() };
                let t_swap = Instant::now();
                let swap_res = ctrl.swap(swap_target, &next, &rt_handle).await;
                swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
                if let Err(e) = swap_res {
                    eprintln!("[swap] failed: {e}");
                    continue;
                }
                swap_total_ms += swap_ms;
                swap_count += 1;
                current = next;
            }

            let (phy, tap, len, step, run, tag, wait, ts, seq) = match &row {
                Some((phy, tap, len, Frame::Z(s, r, g, w, t))) => (
                    *phy, tap.clone(), len.to_string(), s.to_string(), r.to_string(),
                    g.to_string(), w.to_string(), t.to_string(), "-1".to_string(),
                ),
                Some((phy, tap, len, Frame::Unstamped)) => (
                    *phy, tap.clone(), len.to_string(),
                    "-1".into(), "-1".into(), "-1".into(), "-1".into(), "-1".into(),
                    "-1".to_string(),
                ),
                Some((phy, tap, len, Frame::H(seq))) => (
                    *phy, tap.clone(), len.to_string(),
                    "-1".into(), "-1".into(), "-1".into(), "-1".into(), "-1".into(),
                    seq.map_or_else(|| "-1".into(), |s| s.to_string()),
                ),
                None => continue,
            };

            writeln!(
                csv,
                "{rx_idx},{phy},{tap},{frame_event},{len},{step},{run},{tag},{wait},{ts},{seq},{rx_t_ms:.3},{swap_ms:.3}",
            )?;
            csv.flush().ok();
            rx_idx += 1;
        }

        println!(
            "done: Z={n_z} H={n_h}, {swap_count} swaps, avg swap {:.3} ms",
            if swap_count > 0 { swap_total_ms / swap_count as f64 } else { 0.0 },
        );
        println!("csv → {}", csv_path.display());
        Ok(())
    })
}
