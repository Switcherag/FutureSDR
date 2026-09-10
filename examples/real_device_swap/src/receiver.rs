// receiver — load one PHY flow, auto-attach the standard head + tail, run,
// and log every decoded frame to CSV.
//
// Minimal "no-swap" companion to `zigbee_swap` / `ziglow_swap_quicktune`.
// Takes a single PHY flow TOML and starts the canonical three-FG topology:
//
//     sdr_head.toml ─► <user PHY flow> ─► null_tail.toml
//
// The head provides the RF frontend; the tail owns the UDP/RFtap egress,
// the TAP NIC, the per-frame CSV. The user's only job is to pick a PHY.
//
// Output: one row per decoded frame (and per timeout), in the same schema as
// `ziglow_swap.csv` / `ziglow_swap_quicktune.csv`, so `plot_ziglow.py` and the
// other capture scripts read a receiver run without changes. `swap_ms` is
// always -1 here: nothing ever swaps, which is the point of this binary — it
// is the single-PHY baseline the swapping captures are measured against.
//
// Run from this directory so relative TOML paths resolve:
//     cd examples/real_device_swap
//     ../../target/release/receiver --flow flows/zigbee_rxA.toml
//     ../../target/release/receiver --flow flows/halowv6A.toml --csv halow_base.csv
//
// Tap frames (those declared as `[[controller_taps]]` in the chosen PHY
// flow) are printed inline so it's clear the receiver is alive; `--quiet`
// leaves only the CSV.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use futuresdr::runtime::Pmt;
use plugin_host::{FlowgraphController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/null_tail.toml";

/// `[[controller_taps]] name` declared by the Zigbee flows. A frame is
/// attributed to a PHY by tap name, exactly as in the swap binaries, so the
/// `phy` column means the same thing in every capture.
const TAP_Z: &str = "zigbee_frames";

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// What a decoded frame carries, which differs sharply by PHY.
enum Frame {
    /// Z: `step, run, tag, wait_us, ts_us` from the firmware stamp.
    Z(u32, u16, u8, u32, u64),
    /// H: the 802.11 sequence number. RPG payload is pseudo-random, so the
    /// sequence number is the only frame counter available; `None` for a frame
    /// too short or of a type carrying no sequence control.
    H(Option<u16>),
    /// Decoded, but nothing could be read out of it — a Zigbee frame whose
    /// stamp could not be located. Recorded rather than guessed at, and rather
    /// than dropped, so the row count still reflects what the PHY produced.
    Unstamped,
}

#[derive(Parser, Debug)]
#[command(about = "Run a single PHY flow with the standard head + tail wired in, logging frames to CSV.")]
struct Args {
    /// Path to the PHY flow TOML (e.g. flows/zigbee_rxA.toml).
    #[arg(long)]
    flow: PathBuf,

    /// CSV path. Defaults to `receiver_<flow stem>.csv`, so runs of different
    /// flows do not overwrite each other.
    #[arg(long)]
    csv: Option<PathBuf>,

    /// Head flow supplying the RF frontend.
    #[arg(long, default_value = HEAD_FLOW)]
    head: PathBuf,

    /// Permanent tail flow the PHY feeds.
    #[arg(long, default_value = TAIL_FLOW)]
    tail: PathBuf,

    /// Write a `timeout` row if no frame arrives within this many ms, so a
    /// silent band leaves a trace in the capture instead of a gap. 0 disables.
    #[arg(long, default_value_t = 80_000)]
    rx_timeout_ms: u64,

    /// Stop and finalize the CSV after this many rows. 0 = until Ctrl-C.
    #[arg(long, default_value_t = 0)]
    max_frames: u64,

    /// Silence per-frame controller-tap prints. Use when piping to scripts.
    #[arg(long)]
    quiet: bool,
}

/// RFTAP-wrapped blobs carry an 8-byte header (magic, length in 32-bit words,
/// flags, dlt) before the encapsulated frame. Parsing the stamp out of the MAC
/// frame rather than out of the whole blob means the search can never match
/// inside RFTAP's own header.
///
/// Returns the blob unchanged if it is not RFTAP-wrapped, so a tap that
/// delivers a bare frame still works.
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

/// Parse the 19-byte stamp the transmitter puts in the payload:
///
/// ```text
///   [0..4)   frame#   u32   frame number within this IFS step
///   [4..6)   idx      u16   step index
///   [6]      tag      u8    channel number (0x0F = ch15, 0x14 = ch20)
///   [7..11)  ifs_us   u32   IFS of this step
///   [11..19) ts       u64   microseconds since boot
/// ```
///
/// all little-endian, located by the fixed EUI-64 source address that
/// immediately precedes it — identical to `ziglow_swap_quicktune.rs`.
fn parse_zigbee(blob: &[u8]) -> Option<(u32, u16, u8, u32, u64)> {
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

/// Where the CSV goes when `--csv` is not given: `receiver_<flow stem>.csv`.
fn default_csv_path(flow: &PathBuf) -> PathBuf {
    let stem = flow
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "flow".to_string());
    PathBuf::from(format!("receiver_{stem}.csv"))
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();

    let path_str = |p: &PathBuf| -> Result<String, String> {
        p.to_str()
            .map(str::to_string)
            .ok_or_else(|| format!("path is not valid UTF-8: {p:?}"))
    };

    let flow = path_str(&args.flow)?;
    let head = path_str(&args.head)?;
    let tail = path_str(&args.tail)?;
    let csv_path = args.csv.clone().unwrap_or_else(|| default_csv_path(&args.flow));
    let quiet = args.quiet;
    let max_frames = args.max_frames;
    // A disabled timeout is expressed as a deadline that never fires, so the
    // select! below stays one shape either way.
    let rx_timeout = if args.rx_timeout_ms == 0 {
        Duration::from_secs(365 * 24 * 3600)
    } else {
        Duration::from_millis(args.rx_timeout_ms)
    };

    println!("=== receiver — {} ===", flow);
    println!("head: {head}");
    println!("tail: {tail}");
    println!("CSV → {}", csv_path.display());
    if args.rx_timeout_ms == 0 {
        println!("rx timeout: disabled");
    } else {
        println!("rx timeout: {} ms", args.rx_timeout_ms);
    }
    println!();

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(&head)
        .add_permanent(&tail)
        .add_swappable(&flow)
        .tap_channel(256);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Same startup ordering as the swap binaries: head + tail (permanent)
        // first, activate selectors, then start the PHY (swappable slot — we
        // just never swap it).
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

        let mut csv = File::create(&csv_path)?;
        // Same schema as ziglow_swap.csv, so plot_ziglow.py reads either.
        // `phy` is the PHY that decoded the frame (from the tap name); `len` is
        // the one field both PHYs always provide. Stamp columns are Z-only,
        // `seq` is H-only; unavailable fields are -1. `swap_ms` is always -1:
        // this binary holds one flow for the whole run.
        writeln!(
            csv,
            "rx_idx,phy,tap,frame_event,len,step,run,tag,wait_us,ts_us,seq,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut rx_idx: u64 = 0;
        let (mut n_z, mut n_h, mut n_unstamped, mut n_timeout) = (0u64, 0u64, 0u64, 0u64);
        let t0 = Instant::now();

        println!("\nReceiver running. Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            let mut deadline = FutureExt::fuse(Timer::after(rx_timeout));
            let frame_event;
            let mut row: Option<(&'static str, String, usize, Frame)> = None;

            select! {
                _ = deadline => {
                    frame_event = "timeout";
                    n_timeout += 1;
                    println!(
                        "[t={:.1}ms] [timeout {:?}] no frame on {flow}",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        rx_timeout,
                    );
                }
                msg = tap_rx.next().fuse() => match msg {
                    Some((tap_name, pmt)) => {
                        frame_event = "rx";
                        let blob = match pmt {
                            Pmt::Blob(b) => b,
                            other => {
                                if !quiet {
                                    println!("[tap {tap_name}] non-blob pmt: {other:?}");
                                }
                                continue;
                            }
                        };
                        let t = t0.elapsed().as_secs_f64() * 1000.0;

                        // The tap name says which PHY produced this frame.
                        if tap_name == TAP_Z {
                            n_z += 1;
                            let frame = match parse_zigbee(&blob) {
                                Some((step, run, tag, wait_us, ts)) => {
                                    if !quiet {
                                        println!(
                                            "[t={t:.1}ms] [rx Z tap={tap_name} {}B] step={step} run={run} wait_us={wait_us}",
                                            blob.len(),
                                        );
                                    }
                                    Frame::Z(step, run, tag, wait_us, ts)
                                }
                                None => {
                                    n_unstamped += 1;
                                    if !quiet {
                                        println!(
                                            "[t={t:.1}ms] [rx Z tap={tap_name} {}B] no stamp",
                                            blob.len(),
                                        );
                                    }
                                    Frame::Unstamped
                                }
                            };
                            row = Some(("Z", tap_name, blob.len(), frame));
                        } else {
                            // H: random payload, no application marker. Length
                            // and timing are the measurement; the MAC sequence
                            // number comes along for free as a frame counter.
                            n_h += 1;
                            let seq = parse_halow_seq(&blob);
                            if !quiet {
                                println!(
                                    "[t={t:.1}ms] [rx H tap={tap_name} {}B] seq={}",
                                    blob.len(),
                                    seq.map_or_else(|| "-".into(), |s| s.to_string()),
                                );
                            }
                            row = Some(("H", tap_name, blob.len(), Frame::H(seq)));
                        }
                    }
                    None => break,
                },
            }

            let rx_t_ms = t0.elapsed().as_secs_f64() * 1000.0;

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
                None => (
                    "?", String::new(), "-1".into(), "-1".into(), "-1".into(),
                    "-1".into(), "-1".into(), "-1".into(), "-1".into(),
                ),
            };

            // No swap on this path, so the column is a constant -1 rather than
            // a measured latency — same columns, honestly empty.
            writeln!(
                csv,
                "{rx_idx},{phy},{tap},{frame_event},{len},{step},{run},{tag},{wait},{ts},{seq},{rx_t_ms:.3},-1",
            )?;
            csv.flush().ok();
            rx_idx += 1;

            if !quiet && rx_idx % 100 == 0 {
                println!("[stats] {rx_idx} rows, Z={n_z} H={n_h}, {n_timeout} timeouts");
            }
            if max_frames > 0 && rx_idx >= max_frames {
                println!("\nReached --max-frames {max_frames}.");
                break;
            }
        }

        println!(
            "\nFinal: {rx_idx} rows in {:.1} s — Z={n_z} ({n_unstamped} unstamped) H={n_h}, {n_timeout} timeouts",
            t0.elapsed().as_secs_f64(),
        );
        println!("CSV → {}", csv_path.display());
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
}
