// Dynamic PHY TX swap with real SDR output via SeifySink.
//
// Continuously alternates between ZigBee 802.15.4 TX and WLAN 802.11 TX,
// swapping the entire flowgraph each cycle. Runs until Ctrl-C.
//
// The SeifySink (SDR device) is opened ONCE and reused across swaps via
// Flowgraph::take_block / Flowgraph::reuse_block, avoiding the ~2s device
// re-init on every cycle.
//
// ALL blocks are loaded as dynamic plugins at runtime.
// Requires SDR hardware (HackRF, LimeSDR, bladeRF, etc.)

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::prelude::Pmt;
use futuresdr::runtime::{BlockId, Flowgraph, Runtime};
use plugin_host::LoadedPlugin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
struct Args {
    /// Number of frames to send per phase
    #[clap(short, long, default_value_t = 1)]
    n_frames: u64,

    /// Delay between frames (seconds)
    #[clap(long, default_value_t = 0.1)]
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

    /// Post-TX drain time (seconds)
    #[clap(long, default_value_t = 0.5)]
    drain_time: f64,
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
    let mut cycle = 0u64;
    let t_start = Instant::now();
    let mut timings: Vec<(u64, &str, f64, f64, f64, f64, f64)> = Vec::new();

    // ============================================================
    // Open the SDR device ONCE via a throwaway flowgraph
    // ============================================================
    println!("Opening SDR device...");
    let t_dev = Instant::now();
    let snk_block = {
        let mut fg = Flowgraph::new();
        let _snk_id = fg.add_block_dyn(seify_sink.prepare(Box::new((
            args.args.clone(),
            args.zigbee_freq,
            args.zigbee_rate,
            args.gain,
        ))));
        // Don't start — just take the block out
        fg.take_block(BlockId(0)).unwrap()
    };
    println!("SDR device opened in {:.1}ms\n", t_dev.elapsed().as_secs_f64() * 1000.0);

    // ============================================================
    // Continuous swap loop: ZigBee → WLAN → ZigBee → WLAN → ...
    // The SeifySink block is reused across all cycles.
    // ============================================================
    while running.load(Ordering::SeqCst) {
        cycle += 1;

        // --- ZigBee TX phase ---
        println!(
            "=== Cycle {cycle} | ZigBee TX ({} frames @ {:.1} MHz) ===",
            args.n_frames,
            args.zigbee_freq / 1e6,
        );
        let t_phase = Instant::now();
        let t0 = Instant::now();
        let (fg, mac_id) = {
            let mut fg = Flowgraph::new();

            let mac = fg.add_block_dyn(zigbee_mac.prepare(Box::new(())));
            let mac_id: BlockId = mac;
            let modulator = fg.add_block_dyn(zigbee_mod.prepare(Box::new(())));
            let iq_delay = fg.add_block_dyn(zigbee_iq.prepare(Box::new(())));
            let snk = fg.reuse_block(snk_block.clone());

            fg.connect_dyn(mac_id, "output", modulator, "input")?;
            fg.connect_dyn(modulator, "output", iq_delay, "input")?;
            fg.connect_dyn(iq_delay, "output", snk, "inputs[0]")?;
            (fg, mac_id)
        };
        let build_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let (_fg_task, mut handle) = rt.start_sync(fg)?;
        let start_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let t2 = Instant::now();
        let n_frames = args.n_frames;
        let frame_delay = args.frame_delay;
        let drain_time = args.drain_time;

        rt.block_on(async move {
            for seq in 0..n_frames {
                if frame_delay > 0.0 {
                    Timer::after(Duration::from_secs_f64(frame_delay)).await;
                }
                handle
                    .call(
                        mac_id,
                        "tx",
                        Pmt::Blob(
                            format!("ZigBee c{cycle} f{seq}")
                                .as_bytes()
                                .to_vec(),
                        ),
                    )
                    .await
                    .unwrap();
            }
            Timer::after(Duration::from_secs_f64(drain_time)).await;
            handle.terminate_and_wait().await.unwrap();
        });
        let tx_teardown_ms = t2.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_phase.elapsed().as_secs_f64() * 1000.0;
        let tx_ms = (tx_teardown_ms - drain_time * 1000.0).max(0.0);
        let teardown_ms = drain_time * 1000.0;

        println!(
            "  build={:.1}ms  start={:.1}ms  tx={:.1}ms  drain={:.1}ms  total={:.1}ms",
            build_ms, start_ms, tx_ms, teardown_ms, total_ms
        );
        timings.push((cycle, "ZigBee", build_ms, start_ms, tx_ms, teardown_ms, total_ms));

        if !running.load(Ordering::SeqCst) {
            break;
        }

        // --- WLAN TX phase ---
        println!(
            "=== Cycle {cycle} | WLAN TX ({} frames @ {:.1} MHz) ===",
            args.n_frames,
            args.wlan_freq / 1e6,
        );
        let t_phase = Instant::now();
        let t0 = Instant::now();
        let (fg, mac_id) = {
            let mut fg = Flowgraph::new();

            let mac = fg.add_block_dyn(
                wlan_mac.prepare(Box::new(([0x42u8; 6], [0x23u8; 6], [0xffu8; 6]))),
            );
            let mac_id: BlockId = mac;
            let encoder =
                fg.add_block_dyn(wlan_enc.prepare(Box::new("qpsk12".to_string())));
            let encoder_id: BlockId = encoder;
            let mapper = fg.add_block_dyn(wlan_map.prepare(Box::new(())));
            let ifft = fg.add_block_dyn(
                fft.prepare(Box::new((
                    64_usize,
                    true,
                    true,
                    Some((1.0f32 / 52.0).sqrt()),
                ))),
            );
            let prefix =
                fg.add_block_dyn(wlan_pfx.prepare(Box::new((100_usize, 100_usize))));
            let snk = fg.reuse_block(snk_block.clone());

            fg.connect_message(mac_id, "tx", encoder_id, "tx")?;
            fg.connect_dyn(encoder_id, "output", mapper, "input")?;
            fg.connect_dyn(mapper, "output", ifft, "input")?;
            fg.connect_dyn(ifft, "output", prefix, "input")?;
            fg.connect_dyn(prefix, "output", snk, "inputs[0]")?;
            (fg, mac_id)
        };
        let build_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let (_fg_task, mut handle) = rt.start_sync(fg)?;
        let start_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let t2 = Instant::now();
        let n_frames = args.n_frames;
        let frame_delay = args.frame_delay;
        let drain_time = args.drain_time;

        rt.block_on(async move {
            for seq in 0..n_frames {
                if frame_delay > 0.0 {
                    Timer::after(Duration::from_secs_f64(frame_delay)).await;
                }
                handle
                    .call(
                        mac_id,
                        "tx",
                        Pmt::Blob(
                            format!("WLAN c{cycle} f{seq}")
                                .as_bytes()
                                .to_vec(),
                        ),
                    )
                    .await
                    .unwrap();
            }
            Timer::after(Duration::from_secs_f64(drain_time)).await;
            handle.terminate_and_wait().await.unwrap();
        });
        let tx_teardown_ms = t2.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_phase.elapsed().as_secs_f64() * 1000.0;
        let tx_ms = (tx_teardown_ms - drain_time * 1000.0).max(0.0);
        let teardown_ms = drain_time * 1000.0;

        println!(
            "  build={:.1}ms  start={:.1}ms  tx={:.1}ms  drain={:.1}ms  total={:.1}ms\n",
            build_ms, start_ms, tx_ms, teardown_ms, total_ms
        );
        timings.push((cycle, "WLAN", build_ms, start_ms, tx_ms, teardown_ms, total_ms));
    }

    let total = t_start.elapsed();
    println!(
        "Stopped after {cycle} cycles in {:.3}s",
        total.as_secs_f64()
    );

    // ============================================================
    // Write CSV for plotting
    // ============================================================
    let csv_path = "tx_sdr_swap_timings.csv";
    let mut csv = String::from("cycle,phy,build_ms,start_ms,tx_ms,drain_ms,total_ms\n");
    for (cycle, phy, build, start, tx, drain, total) in &timings {
        csv.push_str(&format!(
            "{cycle},{phy},{build:.2},{start:.2},{tx:.2},{drain:.2},{total:.2}\n"
        ));
    }
    std::fs::write(csv_path, &csv)?;
    println!("Timings written to {csv_path}");

    Ok(())
}
