//! 802.11ah listen-only switch with an RFTAP CSV log.
//!
//! Same shape as `listen.rs` — one SDR head, one swappable listen flowgraph,
//! and each decoded frame swaps the listener to the other flow — but it
//! alternates between the two *802.11ah* flows `halow_listenA.toml` and
//! `halow_listenB.toml` (the `halow_listen1/2` pair minus the `blob_to_udp`
//! egress) and, instead of shipping the RFTAP blob to Wireshark over UDP,
//! timestamps it and appends it to a CSV.
//!
//! One decoded frame == one `decoder.rftap` tap message == one CSV row. The
//! timestamp is taken when the *controller* dequeues the tap, so it lags the
//! over-the-air arrival by the DSP + channel latency; it is a receive-order
//! reference, not an air timestamp.
//!
//! Frames that queue up while a swap is in flight are still logged (flagged
//! `phase=swap_drain`), and each row names the flow it came from via the tap
//! name, so a row is attributable even when it lands across a swap boundary.
//!
//! Run (from this directory, after `./build.sh`):
//!   cd examples/dual_phy_handshake
//!   ../../target/release/halow_switch --csv halow_switch.csv

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use futuresdr::futures::StreamExt;
use futuresdr::runtime::Pmt;
use plugin_host::{FlowgraphController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head_listen.toml";
// Everything is indexed [0] = A, [1] = B; each received frame flips the index.
const LISTEN: [&str; 2] = ["flows/halow_listenA.toml", "flows/halow_listenB.toml"];
const NAME: [&str; 2] = ["A", "B"];
// `[[controller_taps]] name` declared by each flow above, same index order.
const TAP: [&str; 2] = ["rftap_a", "rftap_b"];

// `rx_epoch_s` is raw UNIX epoch seconds with nanosecond precision (integer
// seconds + 9 fraction digits, so no f64 rounding); `elapsed_s` is monotonic
// seconds since the log was opened. Formatting into dates is left to whatever
// reads the CSV.
const CSV_HEADER: &str = "seq,rx_epoch_s,elapsed_s,flow,flow_toml,freq_hz,tap,phase,dlt,rftap_len,payload_len,rftap_hex";

#[derive(Parser)]
#[command(about = "Listen-only 802.11ah switch (flow A ↔ B on each frame), logging RFTAP to CSV.")]
struct Args {
    /// CSV file for the RFTAP log; one row per decoded frame.
    #[arg(long, default_value = "halow_switch.csv")]
    csv: PathBuf,
    /// Append to an existing CSV instead of truncating it.
    #[arg(long)]
    append: bool,
    /// Flow to start listening on.
    #[arg(long, default_value = "A", value_parser = ["A", "B"])]
    start: String,
    /// Log only, never swap. The control for the swap's cost: it measures how
    /// many frames one flow decodes when it is left running, so the blind time
    /// a swap adds can be separated from the PHY's own yield.
    #[arg(long)]
    no_swap: bool,
}

/// The `[radio]` section of a listen flow — only the center frequency, which
/// is logged per row so a CSV stays readable after the flows are retuned.
#[derive(serde::Deserialize)]
struct FlowFileRadio {
    radio: RadioSec,
}
#[derive(serde::Deserialize)]
struct RadioSec {
    frequency_hz: f64,
}

fn flow_frequency(toml_path: &str) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(toml_path)?;
    let parsed: FlowFileRadio =
        toml::from_str(&content).map_err(|e| format!("{toml_path} [radio]: {e}"))?;
    Ok(parsed.radio.frequency_hz)
}

/// Split an RFTAP blob into `(dlt, payload)`.
///
/// Layout (see the halowv2 decoder): magic `RFta`, u16 header length in 32-bit
/// words, u16 present-flags, then the optional fields — with bit 0 of the flags
/// set, the first is the u32 DLT.
fn parse_rftap(blob: &[u8]) -> Option<(u32, &[u8])> {
    if blob.len() < 12 || &blob[0..4] != b"RFta" {
        return None;
    }
    let header_len = u16::from_le_bytes(blob[4..6].try_into().ok()?) as usize * 4;
    let present = u16::from_le_bytes(blob[6..8].try_into().ok()?);
    if header_len < 12 || blob.len() < header_len || present & 1 == 0 {
        return None;
    }
    let dlt = u32::from_le_bytes(blob[8..12].try_into().ok()?);
    Some((dlt, &blob[header_len..]))
}

/// Append-only CSV of timestamped RFTAP frames.
struct RftapLog {
    out: BufWriter<std::fs::File>,
    seq: u64,
    t0: Instant,
}

impl RftapLog {
    fn create(path: &Path, append: bool) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let existing = append && path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|e| format!("cannot open '{}': {e}", path.display()))?;
        let mut out = BufWriter::new(file);
        if !existing {
            writeln!(out, "{CSV_HEADER}")?;
            out.flush()?;
        }
        Ok(Self {
            out,
            seq: 0,
            t0: Instant::now(),
        })
    }

    /// Write one row for `blob`, stamped with the current time. Flushed
    /// immediately so the CSV can be tailed while the run is live.
    fn row(
        &mut self,
        flow: usize,
        freq_hz: f64,
        tap: &str,
        phase: &str,
        blob: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let elapsed = self.t0.elapsed().as_secs_f64();

        let (dlt, payload_len) = match parse_rftap(blob) {
            Some((dlt, payload)) => (dlt.to_string(), payload.len().to_string()),
            // Not an RFTAP blob — keep the bytes, leave the parsed fields empty.
            None => (String::new(), String::new()),
        };

        self.seq += 1;
        write!(
            self.out,
            "{},{}.{:09},{:.6},{},{},{:.0},{},{},{},{},{},",
            self.seq,
            epoch.as_secs(),
            epoch.subsec_nanos(),
            elapsed,
            NAME[flow],
            LISTEN[flow],
            freq_hz,
            tap,
            phase,
            dlt,
            blob.len(),
            payload_len,
        )?;
        for b in blob {
            write!(self.out, "{b:02x}")?;
        }
        writeln!(self.out)?;
        self.out.flush()?;
        Ok(())
    }
}

/// Flow index a tap message belongs to, from the tap name declared in the TOML.
/// Robust across a swap boundary, where `cur` no longer describes the frame.
fn flow_of_tap(tap: &str) -> Option<usize> {
    TAP.iter().position(|&t| t == tap)
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();

    let start = usize::from(args.start == "B");
    let freq = [flow_frequency(LISTEN[0])?, flow_frequency(LISTEN[1])?];
    let mut log = RftapLog::create(&args.csv, args.append)?;
    let csv_path = args.csv.display().to_string();
    let no_swap = args.no_swap;

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_swappable(LISTEN[start])
        .tap_channel(64);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Start the permanent head first, then activate selectors, then start
        // the first listener flowgraph.
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

        let listen_idx = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("one swappable listen flowgraph");

        let mut cur = start;
        println!(
            "Listening on {} ({}) @ {:.3} MHz — {}.\n\
             RFTAP log: {csv_path}. Ctrl-C to quit.\n",
            NAME[cur],
            LISTEN[cur],
            freq[cur] / 1e6,
            if no_swap {
                "no-swap baseline, staying on this flow"
            } else {
                "swap on each received frame"
            }
        );

        while let Some((tap_name, pmt)) = tap_rx.next().await {
            let Pmt::Blob(blob) = &pmt else {
                eprintln!("[rx {tap_name}] ignoring non-blob PMT: {pmt:?}");
                continue;
            };
            let flow = flow_of_tap(&tap_name).unwrap_or(cur);
            log.row(flow, freq[flow], &tap_name, "listen", blob)?;

            if no_swap {
                println!(
                    "[rx {tap_name}] frame ({} B) on {} -> logged row {}",
                    blob.len(),
                    NAME[flow],
                    log.seq
                );
                continue;
            }

            let other = cur ^ 1;
            println!(
                "[rx {tap_name}] frame ({} B) on {} -> logged row {}, swapping listener to {}",
                blob.len(),
                NAME[flow],
                log.seq,
                NAME[other]
            );

            ctrl.swap(listen_idx, LISTEN[other], &rt_handle).await?;
            cur = other;

            println!(
                "[swap] now listening on {} ({}) @ {:.3} MHz",
                NAME[cur],
                LISTEN[cur],
                freq[cur] / 1e6
            );

            // Frames that queued while the swap was in progress: log them (the
            // tap name still says which flow decoded them) and drain, so the
            // next await starts from a clean channel.
            while let Ok((queued_tap, queued_pmt)) = tap_rx.try_recv() {
                if let Pmt::Blob(blob) = &queued_pmt {
                    let flow = flow_of_tap(&queued_tap).unwrap_or(cur);
                    log.row(flow, freq[flow], &queued_tap, "swap_drain", blob)?;
                }
            }
        }

        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
}
