// Dynamic PHY TX hot-swap — single flowgraph, zero-downtime switching.
//
// Both ZigBee and WLAN TX chains live in the same flowgraph, connected to
// a Selector block that feeds the SeifySink.  Switching PHY = one message
// to the Selector + freq/rate retune on SeifySink.  The SDR streamer
// NEVER stops.
//
// Architecture:
//   ZigBee MAC → Mod → IQ Delay ──→ Selector(in 0) ──→ SeifySink
//   WLAN MAC → Enc → Map → FFT → Pfx → Selector(in 1) ↗
//
// ALL processing blocks are loaded as dynamic plugins at runtime.
// The Selector block is a built-in FutureSDR block (static).

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::blocks::{Selector, SelectorDropPolicy};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::Pmt;
use futuresdr::runtime::{BlockId, Flowgraph, Runtime};
use plugin_api::LoadedPlugin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
struct Args {
    /// Number of frames to send per phase
    #[clap(short, long, default_value_t = 10)]
    n_frames: u64,

    /// Delay between frames (seconds)
    #[clap(long, default_value_t = 0.0)]
    frame_delay: f64,

    /// Directory containing plugin .so files
    #[clap(long, default_value = ".")]
    plugin_dir: String,

    /// Seify device args (e.g. "driver=hackrf")
    #[clap(short, long, default_value = "")]
    args: String,

    /// TX gain in dB
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,

    /// ZigBee frequency (Hz) — default: channel 26 = 2480 MHz
    #[clap(long, default_value_t = 2.48e9)]
    zigbee_freq: f64,

    /// ZigBee sample rate (Hz)
    #[clap(long, default_value_t = 4e6)]
    zigbee_rate: f64,

    /// WLAN frequency (Hz) — default: channel 6 = 2437 MHz
    #[clap(long, default_value_t = 2.437e9)]
    wlan_freq: f64,

    /// WLAN sample rate (Hz)
    #[clap(long, default_value_t = 20e6)]
    wlan_rate: f64,

    /// Settling time after freq/rate change (seconds)
    #[clap(long, default_value_t = 0.05)]
    settle_time: f64,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let dir = &args.plugin_dir;

    // ============================================================
    // Load ALL plugins once at startup
    // ============================================================
    let zigbee_mac = unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_mac_plugin.so")) };
    let zigbee_mod =
        unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_modulator_plugin.so")) };
    let zigbee_iq =
        unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_iq_delay_plugin.so")) };

    let wlan_mac = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_mac_plugin.so")) };
    let wlan_enc =
        unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_encoder_plugin.so")) };
    let wlan_map = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_mapper_plugin.so")) };
    let fft = unsafe { LoadedPlugin::load(&format!("{dir}/libfft_complex_plugin.so")) };
    let wlan_pfx = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_prefix_plugin.so")) };

    let seify_sink =
        unsafe { LoadedPlugin::load(&format!("{dir}/libseify_sink_plugin.so")) };

    println!("All plugins loaded successfully. Ctrl-C to stop.\n");

    // Ctrl-C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        println!("\nShutting down...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("failed to set Ctrl-C handler");

    let rt = Runtime::new();

    // ============================================================
    // Build ONE flowgraph with both PHY chains + Selector + SeifySink
    // ============================================================
    println!("Building unified flowgraph...");
    let t_build = Instant::now();

    let mut fg = Flowgraph::new();

    // --- ZigBee TX chain ---
    let z_mac = fg.add_block_dyn(zigbee_mac.prepare(Box::new(())));
    let z_mac_id: BlockId = z_mac;
    let z_mod = fg.add_block_dyn(zigbee_mod.prepare(Box::new(())));
    let z_iq = fg.add_block_dyn(zigbee_iq.prepare(Box::new(())));

    // --- WLAN TX chain ---
    let w_mac = fg.add_block_dyn(
        wlan_mac.prepare(Box::new(([0x42u8; 6], [0x23u8; 6], [0xffu8; 6]))),
    );
    let w_mac_id: BlockId = w_mac;
    let w_enc = fg.add_block_dyn(wlan_enc.prepare(Box::new("qpsk12".to_string())));
    let w_enc_id: BlockId = w_enc;
    let w_map = fg.add_block_dyn(wlan_map.prepare(Box::new(())));
    let w_fft = fg.add_block_dyn(
        fft.prepare(Box::new((64_usize, true, true, Some((1.0f32 / 52.0).sqrt())))),
    );
    let w_pfx = fg.add_block_dyn(wlan_pfx.prepare(Box::new((100_usize, 100_usize))));

    // --- Selector: 2 inputs → 1 output (Complex32) ---
    let selector = fg.add_block(Selector::<Complex32, 2, 1>::new(SelectorDropPolicy::DropAll));
    let selector_id: BlockId = selector.into();

    // --- SeifySink (start at ZigBee config) ---
    let snk = fg.add_block_dyn(seify_sink.prepare(Box::new((
        args.args.clone(),
        args.zigbee_freq,
        args.zigbee_rate,
        args.gain,
    ))));
    let snk_id: BlockId = snk;

    // --- Wire ZigBee chain → Selector input 0 ---
    fg.connect_dyn(z_mac_id, "output", z_mod, "input")?;
    fg.connect_dyn(z_mod, "output", z_iq, "input")?;
    fg.connect_dyn(z_iq, "output", selector_id, "inputs[0]")?;

    // --- Wire WLAN chain → Selector input 1 ---
    fg.connect_message(w_mac_id, "tx", w_enc_id, "tx")?;
    fg.connect_dyn(w_enc_id, "output", w_map, "input")?;
    fg.connect_dyn(w_map, "output", w_fft, "input")?;
    fg.connect_dyn(w_fft, "output", w_pfx, "input")?;
    fg.connect_dyn(w_pfx, "output", selector_id, "inputs[1]")?;

    // --- Wire Selector → SeifySink ---
    fg.connect_dyn(selector_id, "outputs[0]", snk_id, "inputs[0]")?;

    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
    println!("Flowgraph built in {build_ms:.1}ms");

    // ============================================================
    // Start the flowgraph ONCE
    // ============================================================
    println!("Starting flowgraph (SDR device opens here)...");
    let t_start = Instant::now();
    let (_fg_task, mut handle) = rt.start_sync(fg)?;
    let start_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    println!("Flowgraph started in {start_ms:.1}ms\n");

    // ============================================================
    // Continuous hot-swap loop: ZigBee ↔ WLAN
    // ============================================================
    let t_total = Instant::now();
    let n_frames = args.n_frames;
    let frame_delay = args.frame_delay;
    let settle_time = args.settle_time;
    let zigbee_freq = args.zigbee_freq;
    let zigbee_rate = args.zigbee_rate;
    let wlan_freq = args.wlan_freq;
    let wlan_rate = args.wlan_rate;

    let (cycle, timings) = rt.block_on(async move {
        let mut cycle = 0u64;
        let mut timings: Vec<(u64, String, f64, f64, f64)> = Vec::new();

        while running.load(Ordering::SeqCst) {
            cycle += 1;

            // --- ZigBee phase ---
            let t_phase = Instant::now();
            let t_switch = Instant::now();

            handle.callback(selector_id, "input_index", Pmt::Usize(0)).await.unwrap();
            let sel_ms = t_switch.elapsed().as_secs_f64() * 1000.0;
            handle.callback(snk_id, "freq", Pmt::F64(zigbee_freq)).await.unwrap();
            handle.callback(snk_id, "sample_rate", Pmt::F64(zigbee_rate)).await.unwrap();
            let switch_ms = t_switch.elapsed().as_secs_f64() * 1000.0;

            if settle_time > 0.0 {
                Timer::after(Duration::from_secs_f64(settle_time)).await;
            }

            let t_tx = Instant::now();
            for seq in 0..n_frames {
                if frame_delay > 0.0 {
                    Timer::after(Duration::from_secs_f64(frame_delay)).await;
                }
                handle
                    .call(
                        z_mac_id,
                        "tx",
                        Pmt::Blob(format!("ZigBee c{cycle} f{seq}").as_bytes().to_vec()),
                    )
                    .await
                    .unwrap();
            }
            let tx_ms = t_tx.elapsed().as_secs_f64() * 1000.0;
            let total_ms = t_phase.elapsed().as_secs_f64() * 1000.0;

            println!(
                "Cycle {cycle:3} | ZigBee | sel={sel_ms:.2}ms  retune={:.2}ms  tx={tx_ms:.1}ms  total={total_ms:.1}ms",
                switch_ms - sel_ms
            );
            timings.push((cycle, "ZigBee".into(), switch_ms, tx_ms, total_ms));

            if !running.load(Ordering::SeqCst) {
                break;
            }

            // --- WLAN phase ---
            let t_phase = Instant::now();
            let t_switch = Instant::now();

            handle.callback(selector_id, "input_index", Pmt::Usize(1)).await.unwrap();
            let sel_ms = t_switch.elapsed().as_secs_f64() * 1000.0;
            handle.callback(snk_id, "freq", Pmt::F64(wlan_freq)).await.unwrap();
            handle.callback(snk_id, "sample_rate", Pmt::F64(wlan_rate)).await.unwrap();
            let switch_ms = t_switch.elapsed().as_secs_f64() * 1000.0;

            if settle_time > 0.0 {
                Timer::after(Duration::from_secs_f64(settle_time)).await;
            }

            let t_tx = Instant::now();
            for seq in 0..n_frames {
                if frame_delay > 0.0 {
                    Timer::after(Duration::from_secs_f64(frame_delay)).await;
                }
                handle
                    .call(
                        w_mac_id,
                        "tx",
                        Pmt::Blob(format!("WLAN c{cycle} f{seq}").as_bytes().to_vec()),
                    )
                    .await
                    .unwrap();
            }
            let tx_ms = t_tx.elapsed().as_secs_f64() * 1000.0;
            let total_ms = t_phase.elapsed().as_secs_f64() * 1000.0;

            println!(
                "Cycle {cycle:3} | WLAN   | sel={sel_ms:.2}ms  retune={:.2}ms  tx={tx_ms:.1}ms  total={total_ms:.1}ms\n",
                switch_ms - sel_ms
            );
            timings.push((cycle, "WLAN".into(), switch_ms, tx_ms, total_ms));
        }

        handle.terminate_and_wait().await.unwrap();
        (cycle, timings)
    });

    let total = t_total.elapsed();
    println!(
        "Stopped after {cycle} cycles in {:.3}s",
        total.as_secs_f64()
    );

    // ============================================================
    // Summary
    // ============================================================
    if !timings.is_empty() {
        let switches: Vec<f64> = timings.iter().map(|t| t.2).collect();
        let avg_switch = switches.iter().sum::<f64>() / switches.len() as f64;
        let max_switch = switches.iter().cloned().fold(0.0f64, f64::max);
        println!("\nSwitch time: avg={avg_switch:.2}ms  max={max_switch:.2}ms  ({} switches)", switches.len());
    }

    // Write CSV
    let csv_path = "tx_sdr_hotswap_timings.csv";
    let mut csv = String::from("cycle,phy,switch_ms,tx_ms,total_ms\n");
    for (cycle, phy, switch, tx, total) in &timings {
        csv.push_str(&format!("{cycle},{phy},{switch:.3},{tx:.2},{total:.2}\n"));
    }
    std::fs::write(csv_path, &csv)?;
    println!("Timings written to {csv_path}");

    Ok(())
}
