// Real-device PHY swap — auto-rotate flows every SWAP_PERIOD.
//
// All radio + flowgraph wiring is described inside each TOML. Main has no
// idea a RadioController exists: it just lists TOML paths and rotates.
//
// TOML contract (predefined API):
//   [radio]                       → consumed by FlowgraphController
//     frequency_hz   = 2.405e9    (required, retuned on swap)
//     sample_rate_hz = 4e6        (required at startup, fixed afterwards)
//     gain_db        = 60.0       (required, retuned on swap)
//     hardware_rate_hz = 20e6     (optional, default 20e6)
//     device_args    = ""         (optional, default "")
//   [[ports]] / [[blocks]] / [[connections]]   → flowgraph topology
//   [[controller_taps]]                        → tap PMTs back to controller
//
// On startup the controller scans the listed TOMLs; the first complete
// `[radio]` it finds provisions a RadioController and wires it to that
// flowgraph's first c32 input port. Subsequent swaps re-apply
// `frequency_hz` and `gain_db` automatically.
//
// Usage:
//   cargo build --release -p real-device-swap-example \
//     -p seify_source_plugin -p fir_resampler_plugin \
//     -p zigbee_demod_plugin -p zigbee_decoder_plugin -p zigbee_mac_plugin \
//     -p clock_recovery_mm_plugin -p null_sink_plugin -p blob_to_udp_plugin \
//     -p wlan_dc_offset_plugin -p wlan_ah_sync_short_plugin -p wlan_ah_sync_long_plugin \
//     -p wlan_ah_frame_equalizer_plugin -p wlan_ah_decoder_plugin \
//     -p wlan_ah_v2_stf_detector_plugin -p wlan_ah_v2_cfo_corrector_plugin \
//     -p wlan_ah_v2_sto_corrector_plugin -p wlan_ah_v2_channel_estimator_plugin \
//     -p wlan_ah_v2_sig_decoder_plugin -p wlan_ah_v2_data_demod_plugin \
//     -p delay_complex_plugin -p complex_to_mag2_plugin \
//     -p moving_average_f32_plugin -p moving_average_complex_plugin \
//     -p mult_conj_plugin -p divide_mag_plugin -p fft_complex_plugin
//
//   cd examples/real_device_swap
//   ../../target/release/real_device_swap
//
// Observe received frames:
//   nc -u -l 55555   (or tcpdump -i lo -A udp port 55555)

use std::time::Duration;

use futuresdr::async_io::Timer;
use futuresdr::futures::{FutureExt, StreamExt, select};
use plugin_host::{FlowgraphController, default_plugin_dir};

/// Auto-rotation sequence: every SWAP_PERIOD, swap to the next entry.
const ROTATION: &[&str] = &[
    "flows/zigbee_rx.toml",
    "flows/wlan_ah_rx_v2.toml",
];

const SWAP_PERIOD: Duration = Duration::from_secs(9);

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    if ROTATION.is_empty() {
        return Err("ROTATION is empty".into());
    }

    println!("=== real_device_swap — auto-rotate every {}s ===", SWAP_PERIOD.as_secs());
    println!("Rotation: {ROTATION:?}\n");

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head("flows/sdr_head.toml")
        .add_swappable(ROTATION[0])
        .tap_channel(64);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Start permanent FGs first (the radio head), then swappables.
        for &(idx, ref path, permanent) in &entries {
            if permanent {
                println!("Starting head fg/{idx}/ from '{path}' ...");
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, permanent) in &entries {
            if !permanent {
                println!("Starting fg/{idx}/ from '{path}' ...");
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }
        println!("\nReceiver running. Frames are sent to UDP 127.0.0.1:55555.");
        println!("Press Ctrl-C to quit.\n");

        if ROTATION.len() < 2 {
            eprintln!(
                "[policy] rotation has < 2 entries; staying on '{}' forever",
                ROTATION[0]
            );
            while let Some((tap_name, pmt)) = tap_rx.next().await {
                println!("[tap] {tap_name}: {pmt:?}");
            }
            return Ok::<(), Box<dyn std::error::Error + Send + Sync>>(());
        }

        // Timer-driven swap loop. `ctrl.swap()` re-applies the new TOML's
        // `[radio] frequency_hz` / `gain_db` automatically — for the
        // head-FG path that means dispatching live messages to the head's
        // sdr_block. `select!` drains the tap channel between swaps so
        // received frames still print.
        let swap_target = entries.iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("at least one swappable flowgraph");
        let mut rot_idx = 0usize;

        loop {
            // Timer impls both Future + Stream, so disambiguate `.fuse()`.
            let mut deadline = FutureExt::fuse(Timer::after(SWAP_PERIOD));
            loop {
                select! {
                    _ = deadline => break,
                    msg = tap_rx.next().fuse() => match msg {
                        Some((tap_name, pmt)) => println!("[tap] {tap_name}: {pmt:?}"),
                        None => break,
                    },
                }
            }

            rot_idx = (rot_idx + 1) % ROTATION.len();
            let next = ROTATION[rot_idx];
            println!("[policy] swapping to '{next}'");
            if let Err(e) = ctrl.swap(swap_target, next, &rt_handle).await {
                eprintln!("[policy] swap failed: {e}");
            }
        }
    })
}
