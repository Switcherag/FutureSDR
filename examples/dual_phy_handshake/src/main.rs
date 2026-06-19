//! Dual-PHY handshake (controller-managed) — one SDR answers an 802.15.4 frame
//! with a custom ACK, then *swaps PHY* and answers again on 802.11ah.
//!
//! Topology, built on the [`plugin_host`] flowgraph manager (same machinery as
//! `real_device_swap`), with TWO radios registered in the controller:
//!
//!   RX radio (head):   SeifySource → resampler → [bridge] → zigbee_listen PHY
//!                      every decoded frame → controller tap → policy loop
//!   TX radio ("tx"):   [tx_buf] → BridgeSourceC32 → SeifySink   (RadioController::new_sink)
//!   ACK block:         loads ack_zigbee.cf32 / ack_halow.cf32, and on `trigger(i)`
//!                      pushes waveform i into the TX radio's input buffer
//!
//! Handshake policy (the `run_with` async loop): on each listened frame —
//!   1. trigger the 802.15.4 ACK (TX already on the zigbee band),
//!   2. retune the "tx" radio to the 802.11ah band ("swap PHY"),
//!   3. trigger the 802.11ah ACK,
//!   4. retune "tx" back to the listen band for the next handshake.
//!
//! ACK waveforms are precomputed by the `gen_waveforms` side binary — there is
//! no live TX PHY here; a swap is just a retune + a different waveform index.
//!
//! Run (from this directory, after `./build.sh` and `gen_waveforms`):
//!   cd examples/dual_phy_handshake
//!   ../../target/release/gen_waveforms
//!   ../../target/release/dual_phy_handshake

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
const LISTEN_FLOW: &str = "flows/zigbee_listen.toml";

/// Waveform indices inside the ACK block (order matches `from_files` below).
const ZIGBEE_WF: usize = 0;
const HALOW_WF: usize = 1;

#[derive(Parser, Debug)]
#[command(about = "Controller-managed single-SDR dual-PHY (802.15.4 + 802.11ah) ACK handshake.")]
struct Args {
    /// seify device args for the TX sink radio. Empty = first device (must be
    /// the same hardware the head TOML opens). bladeRF: full-duplex RX + TX.
    #[arg(long, default_value = "")]
    device: String,
    /// Listen + 802.15.4 ACK center frequency (Hz). Must match the head TOML.
    #[arg(long, default_value_t = 2.405e9)]
    zigbee_freq: f64,
    /// 802.11ah ACK center frequency (Hz).
    #[arg(long, default_value_t = 918.4e6)]
    halow_freq: f64,
    /// TX sample rate (Hz). Must match the rate the ACK waveforms were
    /// generated at (gen_waveforms default = 4 MHz).
    #[arg(long, default_value_t = 4e6)]
    sample_rate: f64,
    /// TX gain (dB).
    #[arg(long, default_value_t = 60.0)]
    tx_gain: f64,
    /// Precomputed 802.15.4 ACK waveform (cf32).
    #[arg(long, default_value = "ack_zigbee.cf32")]
    zigbee_ack: PathBuf,
    /// Precomputed 802.11ah ACK waveform (cf32).
    #[arg(long, default_value = "ack_halow.cf32")]
    halow_ack: PathBuf,
    /// Extra dwell after a burst's nominal length, in ms (covers sink buffering).
    #[arg(long, default_value_t = 5)]
    post_ack_guard_ms: u64,
    /// Allowance for the TX LO to settle after a retune, in ms.
    #[arg(long, default_value_t = 15)]
    retune_settle_ms: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();
    println!("Configuration: {args:?}\n");

    let plugin_dir = default_plugin_dir();

    // ── TX sink radio: [tx_buf] → BridgeSourceC32 → SeifySink ──
    // Armed on the zigbee band; retuned to halow mid-handshake.
    let (tx_radio, tx_buf) = RadioController::new_sink(
        &plugin_dir,
        &args.device,
        args.zigbee_freq,
        args.sample_rate,
        args.tx_gain,
    );

    // ── ACK block: load precomputed waveforms, bound to the TX radio's buffer ──
    let ack = AckBurst::from_files(&[args.zigbee_ack.clone(), args.halow_ack.clone()], tx_buf)?;
    let lens = ack.waveform_lens();
    let zigbee_burst = Duration::from_secs_f64(lens[ZIGBEE_WF] as f64 / args.sample_rate);
    let halow_burst = Duration::from_secs_f64(lens[HALOW_WF] as f64 / args.sample_rate);
    println!(
        "ACK waveforms: 802.15.4 {} samples ({:.2} ms), 802.11ah {} samples ({:.2} ms)",
        lens[ZIGBEE_WF],
        zigbee_burst.as_secs_f64() * 1e3,
        lens[HALOW_WF],
        halow_burst.as_secs_f64() * 1e3,
    );

    // ── Controller: RX head + listen PHY + registered TX radio ──
    let (builder, mut tap_rx) = FlowgraphController::builder(plugin_dir)
        .add_head(HEAD_FLOW)
        .add_swappable(LISTEN_FLOW)
        .register_radio("tx", tx_radio)
        .tap_channel(64);

    let zigbee_freq = args.zigbee_freq;
    let halow_freq = args.halow_freq;
    let post_guard = Duration::from_millis(args.post_ack_guard_ms);
    let settle = Duration::from_millis(args.retune_settle_ms);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Start permanent (RX head) → activate selectors → start swappable (listen PHY).
        // start_radio (called by run_with before this closure) already started the TX radio.
        for &(idx, ref path, perm) in &entries {
            if perm {
                println!("Starting head fg/{idx}/ from '{path}'");
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                println!("Starting listen fg/{idx}/ from '{path}'");
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        // ── ACK flowgraph: a single message-driven block feeding tx_buf ──
        let mut ack_fg = Flowgraph::new();
        let ack_ref = ack_fg.add_block(ack);
        let ack_id: BlockId = (&ack_ref).into();
        let mut ack_handle = rt_handle.start(ack_fg).await?;

        println!(
            "\nHandshake ready. Listening for 802.15.4 frames @ {:.3} MHz.",
            zigbee_freq / 1e6
        );
        println!(
            "On each frame: ACK 802.15.4, swap PHY, ACK 802.11ah @ {:.3} MHz. Ctrl-C to quit.\n",
            halow_freq / 1e6
        );

        while let Some((tap_name, pmt)) = tap_rx.next().await {
            let n = match &pmt {
                Pmt::Blob(b) => b.len(),
                _ => 0,
            };
            println!("[rx {tap_name}] frame ({n} B) → dual-PHY ACK handshake");

            // ACK #1 — 802.15.4 (TX already armed on the zigbee band).
            ack_handle.call(ack_id, "trigger", Pmt::Usize(ZIGBEE_WF)).await?;
            Timer::after(zigbee_burst + post_guard).await;

            // Swap PHY: retune the TX radio to the 802.11ah band.
            if let Some(tx) = ctrl.radio("tx") {
                tx.set_frequency(halow_freq).await?;
            }
            Timer::after(settle).await;

            // ACK #2 — 802.11ah.
            ack_handle.call(ack_id, "trigger", Pmt::Usize(HALOW_WF)).await?;
            Timer::after(halow_burst + post_guard).await;

            // Re-arm: back to the listen/zigbee band for the next handshake.
            if let Some(tx) = ctrl.radio("tx") {
                tx.set_frequency(zigbee_freq).await?;
            }
            Timer::after(settle).await;

            println!(
                "[tx] handshake done — ACK 802.15.4 @ {:.1} MHz, ACK 802.11ah @ {:.1} MHz",
                zigbee_freq / 1e6,
                halow_freq / 1e6
            );

            // Drop frames that piled up during the handshake; react to fresh ones.
            while tap_rx.try_recv().is_ok() {}
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;

    Ok(())
}
