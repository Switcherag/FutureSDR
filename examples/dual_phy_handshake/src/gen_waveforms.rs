//! Side execution: precompute the two ACK waveforms and write them to disk.
//!
//! The runtime handshake (`dual_phy_handshake`) does NOT modulate ACKs live —
//! it replays IQ produced here. This keeps PHY processing off the latency path
//! and makes a "PHY swap" just `retune TX + pick the other file`.
//!
//! Each ACK is generated once by running the repository's real TX PHY (see
//! `waveforms.rs`) and written as a `.cf32` file: interleaved little-endian
//! `f32` pairs (I, Q, I, Q, …) — the GNU Radio "complex float 32" convention,
//! which the runtime ACK block (`ack_burst.rs`) reads back.
//!
//! Run once (no SDR needed — pure DSP):
//!     cargo run --release -p dual-phy-handshake --bin gen_waveforms
//! Output: ack_zigbee.cf32, ack_halow.cf32 in the current directory (override
//! with --out-dir).

mod waveforms;

use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use futuresdr::prelude::Complex32;
use futuresdr::runtime::Runtime;
use halowv2::Mcs;

#[derive(Parser, Debug)]
#[command(about = "Precompute 802.15.4 + 802.11ah ACK waveforms into .cf32 files.")]
struct Args {
    /// Directory to write ack_zigbee.cf32 / ack_halow.cf32 into.
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,
    /// ACK payload bytes (ASCII).
    #[arg(long, default_value = "ACK")]
    payload: String,
}

/// Write interleaved little-endian f32 I/Q to `path` (GNU Radio cf32).
fn write_cf32(path: &PathBuf, wave: &[Complex32]) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(wave.len() * 8);
    for c in wave {
        buf.extend_from_slice(&c.re.to_le_bytes());
        buf.extend_from_slice(&c.im.to_le_bytes());
    }
    let mut f = std::fs::File::create(path)
        .map_err(|e| anyhow::anyhow!("create {}: {e}", path.display()))?;
    f.write_all(&buf)?;
    f.flush()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.out_dir)?;

    let rt = Runtime::new();
    let payload = args.payload.as_bytes();

    println!("Precomputing 802.15.4 ACK waveform ...");
    let zigbee = waveforms::zigbee_ack(&rt, payload)?;
    let zigbee_path = args.out_dir.join("ack_zigbee.cf32");
    write_cf32(&zigbee_path, &zigbee)?;
    println!("  {} samples -> {}", zigbee.len(), zigbee_path.display());

    println!("Precomputing 802.11ah ACK waveform ...");
    let halow = waveforms::halow_ack(&rt, payload, Mcs::Qpsk_1_2)?;
    let halow_path = args.out_dir.join("ack_halow.cf32");
    write_cf32(&halow_path, &halow)?;
    println!("  {} samples -> {}", halow.len(), halow_path.display());

    println!("\nDone. Feed these to the runtime via --zigbee-ack / --halow-ack.");
    Ok(())
}
