//! Dual-PHY ACK with an alternating listener.
//!
//! Starts listening on 802.15.4. On each decoded frame: ACK on the listen PHY,
//! retune to the other band and ACK there, then *swap the listener flowgraph*
//! to the other PHY for the next frame. One full-duplex SDR: the radio head
//! (`sdr_head_listen.toml`, an RX source plus a `[tx]` section for the transmit
//! sink) drives both directions. ACKs are precomputed waveforms (see
//! `gen_waveforms`) replayed by `AckBurst`.
//!
//! The RX and TX LO are shared on the device, so a *single* head retune moves
//! both. ACK #2's band change is done once (via the head) right after ACK #1's
//! burst has gone out — the earliest moment that won't corrupt ACK #1 — and the
//! subsequent listener swap reuses it (its own retune is skipped as
//! already-applied). No separate TX retune is issued.
//!
//! Run (from this directory, after `./build.sh` and `gen_waveforms`):
//!   cd examples/dual_phy_handshake
//!   ../../target/release/ack

mod ack_burst;

use std::path::PathBuf;
use std::time::Duration;

use ack_burst::AckBurst;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futures::StreamExt;
use futuresdr::prelude::*;
use plugin_host::{FlowgraphController, RadioController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head_listen.toml";
// Everything is indexed [0] = 802.15.4, [1] = 802.11ah; a handshake flips the index.
const LISTEN: [&str; 2] = ["flows/zigbee_listen.toml", "flows/halow_listen.toml"];
// TX ACK center freq per band — MUST match the `[radio] frequency_hz` in each
// LISTEN flow above (the precomputed ACK carries no carrier; the radio sets it).
const FREQ: [f64; 2] = [2.425e9, 918.4e6]; // halow = EU upper-900 band 917.4–919.4 MHz (center)
const NAME: [&str; 2] = ["802.15.4", "802.11ah"];
const GUARD: Duration = Duration::from_millis(5); // sink-buffering slack after a burst
const SETTLE: Duration = Duration::from_millis(15); // TX LO retune settle

#[derive(Parser)]
#[command(about = "Single-SDR dual-PHY ACK; listener alternates 802.15.4 ↔ 802.11ah each frame.")]
struct Args {
    /// Precomputed 802.15.4 ACK waveform (cf32).
    #[arg(long, default_value = "ack_zigbee.cf32")]
    zigbee_ack: PathBuf,
    /// Precomputed 802.11ah ACK waveform (cf32).
    #[arg(long, default_value = "ack_halow.cf32")]
    halow_ack: PathBuf,
}

/// The radio head's `[tx]` section — the transmit sink's device/rate/gain.
/// (Center frequency tracks the shared RX/TX LO, set via the head retune.)
#[derive(serde::Deserialize)]
struct HeadFileTx {
    tx: TxSec,
}
#[derive(serde::Deserialize)]
struct TxSec {
    #[serde(default)]
    device_args: String,
    sample_rate_hz: f64,
    gain_db: f64,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();
    let plugin_dir = default_plugin_dir();

    // TX sink config from the radio head's [tx] section (single source of truth).
    let tx: TxSec = toml::from_str::<HeadFileTx>(&std::fs::read_to_string(HEAD_FLOW)?)
        .map_err(|e| format!("{HEAD_FLOW} [tx]: {e}"))?
        .tx;

    // TX sink radio, armed on the 802.15.4 band (the first listen PHY). It is
    // never retuned directly: the RX/TX LO is shared, so the head retune (in the
    // loop below) moves the TX carrier too.
    let (tx_radio, tx_buf) =
        RadioController::new_sink(&plugin_dir, &tx.device_args, FREQ[0], tx.sample_rate_hz, tx.gain_db);

    // Precomputed ACK waveforms; index matches FREQ/LISTEN (0 = zigbee, 1 = halow).
    let ack = AckBurst::from_files(&[args.zigbee_ack, args.halow_ack], tx_buf)?;
    let len = ack.waveform_lens();
    let burst = [
        Duration::from_secs_f64(len[0] as f64 / tx.sample_rate_hz),
        Duration::from_secs_f64(len[1] as f64 / tx.sample_rate_hz),
    ];

    let (builder, mut tap_rx) = FlowgraphController::builder(plugin_dir)
        .add_head(HEAD_FLOW)
        .add_swappable(LISTEN[0])
        .register_radio("tx", tx_radio)
        .tap_channel(64);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Bring up the RX head, then the first (802.15.4) listen flowgraph.
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

        // The ACK block lives in its own one-block flowgraph feeding tx_buf.
        let mut ack_fg = Flowgraph::new();
        let ack_ref = ack_fg.add_block(ack);
        let ack_id: BlockId = (&ack_ref).into();
        let mut ack_handle = rt_handle.start(ack_fg).await?;

        let mut cur = 0usize; // 0 = 802.15.4, 1 = 802.11ah; TX armed on FREQ[0] at startup.
        println!("Listening on {} @ {:.3} MHz. Ctrl-C to quit.\n", NAME[cur], FREQ[cur] / 1e6);

        while let Some((tap, _)) = tap_rx.next().await {
            let other = cur ^ 1;
            println!("[rx {tap}] frame → ACK {} then ACK {}", NAME[cur], NAME[other]);

            // ACK #1 — listen band. The shared LO is already on FREQ[cur] (from
            // startup, or the previous frame's head retune), so fire immediately.
            ack_handle.call(ack_id, "trigger", Pmt::Usize(cur)).await?;

            // ACK #2 — other band. ONE retune: move the shared RX/TX LO via the
            // head. Done as early as is safe — only once ACK #1's burst has fully
            // gone out (retuning mid-burst would corrupt it), i.e. right after
            // the first transmit. With the LO shared, this moves the TX carrier;
            // the listener swap below then reuses it (its retune is skipped).
            Timer::after(burst[cur] + GUARD).await;
            ctrl.set_frequency(FREQ[other]).await?;
            Timer::after(SETTLE).await;
            ack_handle.call(ack_id, "trigger", Pmt::Usize(other)).await?;
            Timer::after(burst[other] + GUARD).await;

            // Swap the listener DSP to the other PHY. The head is already on
            // FREQ[other] from the retune above, so swap()'s own retune no-ops.
            ctrl.swap(listen_idx, LISTEN[other], &rt_handle).await?;
            cur = other;

            println!("[swap] now listening on {} @ {:.3} MHz\n", NAME[cur], FREQ[cur] / 1e6);
            while tap_rx.try_recv().is_ok() {} // drop frames queued during the handshake
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
}
