// receiver — load one PHY flow, auto-attach the standard head + tail, run.
//
// Minimal "no-swap" companion to `zigbee_swap` / `per_frame_swap`. Takes
// a single PHY flow TOML and starts the canonical three-FG topology:
//
//     sdr_head.toml ─► <user PHY flow> ─► network_tail.toml
//
// The head provides the RF frontend; the tail owns the UDP/RFtap egress,
// the TAP NIC, the per-frame CSV. The user's only job is to pick a PHY.
//
// Run from this directory so relative TOML paths resolve:
//     cd examples/real_device_swap
//     ../../target/release/receiver --flow flows/zigbee_rxA.toml
//
// Tap frames (those declared as `[[controller_taps]]` in the chosen PHY
// flow) are printed inline so it's clear the receiver is alive.

use std::path::PathBuf;

use clap::Parser;
use futuresdr::futures::StreamExt;
use plugin_host::{FlowgraphController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head.toml";
const TAIL_FLOW: &str = "flows/network_tail.toml";

#[derive(Parser, Debug)]
#[command(about = "Run a single PHY flow with the standard head + tail wired in.")]
struct Args {
    /// Path to the PHY flow TOML (e.g. flows/zigbee_rxA.toml).
    #[arg(long)]
    flow: PathBuf,

    /// Silence per-frame controller-tap prints. Use when piping to scripts.
    #[arg(long)]
    quiet: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    let args = Args::parse();

    let flow = args
        .flow
        .to_str()
        .ok_or_else(|| format!("flow path is not valid UTF-8: {:?}", args.flow))?
        .to_string();
    let quiet = args.quiet;

    println!("=== receiver — {} ===", flow);
    println!("head: {HEAD_FLOW}");
    println!("tail: {TAIL_FLOW}");
    println!();

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_permanent(TAIL_FLOW)
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

        println!("\nReceiver running. Press Ctrl-C to quit.\n");

        while let Some((tap_name, pmt)) = tap_rx.next().await {
            if !quiet {
                println!("[tap {tap_name}] {pmt:?}");
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
}
