// Dynamic PHY TX swap — demonstrates switching from ZigBee TX to WLAN TX.
//
// Phase 1: Builds a ZigBee 802.15.4 TX flowgraph, sends N frames, terminates.
// Phase 2: Builds a WLAN 802.11 TX flowgraph, sends N frames, terminates.
//
// The NullSink is created ONCE and reused across phases via reuse_block().
// ALL blocks are loaded as dynamic plugins at runtime.

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::prelude::Pmt;
use futuresdr::runtime::{BlockId, Flowgraph, Runtime};
use plugin_host::LoadedPlugin;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
struct Args {
    /// Number of frames to send per phase
    #[clap(short, long, default_value_t = 1)]
    n_frames: u64,

    /// Delay between frames (seconds)
    #[clap(long, default_value_t = 0.0)]
    frame_delay: f64,

    /// Directory containing plugin .so files
    #[clap(long, default_value = ".")]
    plugin_dir: String,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let dir = &args.plugin_dir;

    // ZigBee TX plugins
    let zigbee_mac = unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_mac_plugin.so")) };
    let zigbee_mod =
        unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_modulator_plugin.so")) };
    let zigbee_iq =
        unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_iq_delay_plugin.so")) };

    // WLAN TX plugins
    let wlan_mac = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_mac_plugin.so")) };
    let wlan_enc =
        unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_encoder_plugin.so")) };
    let wlan_map = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_mapper_plugin.so")) };
    let fft = unsafe { LoadedPlugin::load(&format!("{dir}/libfft_complex_plugin.so")) };
    let wlan_pfx = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_prefix_plugin.so")) };

    // Shared
    let null_sink = unsafe { LoadedPlugin::load(&format!("{dir}/libnull_sink_plugin.so")) };

    println!("All plugins loaded successfully");

    let rt = Runtime::new();

    // Create NullSink ONCE and reuse across phases
    let snk_block = {
        let mut fg = Flowgraph::new();
        fg.add_block_dyn(null_sink.prepare(Box::new("c32".to_string())));
        fg.take_block(BlockId(0)).unwrap()
    };

    // ============================================================
    // Phase 1: ZigBee TX (reuse NullSink)
    // ============================================================
    println!(
        "\n=== Phase 1: ZigBee 802.15.4 TX ({} frames) ===",
        args.n_frames
    );
    let t_zigbee = Instant::now();
    {
        let mut fg = Flowgraph::new();

        let mac = fg.add_block_dyn(zigbee_mac.prepare(Box::new(())));
        let mac_id: BlockId = mac;
        let modulator = fg.add_block_dyn(zigbee_mod.prepare(Box::new(())));
        let iq_delay = fg.add_block_dyn(zigbee_iq.prepare(Box::new(())));
        let snk = fg.reuse_block(snk_block.clone());

        fg.connect_dyn(mac_id, "output", modulator, "input")?;
        fg.connect_dyn(modulator, "output", iq_delay, "input")?;
        fg.connect_dyn(iq_delay, "output", snk, "input")?;

        let (_fg_task, mut handle) = rt.start_sync(fg)?;

        let n_frames = args.n_frames;
        let frame_delay = args.frame_delay;

        rt.block_on(async move {
            for seq in 0..n_frames {
                Timer::after(Duration::from_secs_f64(frame_delay)).await;
                handle
                    .call(
                        mac_id,
                        "tx",
                        Pmt::Blob(format!("ZigBee frame {seq}").as_bytes().to_vec()),
                    )
                    .await
                    .unwrap();
            }
            Timer::after(Duration::from_secs_f64(0.2)).await;
            handle.terminate_and_wait().await.unwrap();
        });
    }
    let zigbee_elapsed = t_zigbee.elapsed();
    println!("ZigBee TX done in {:.3}s", zigbee_elapsed.as_secs_f64());

    // ============================================================
    // Phase 2: WLAN TX (reuse same NullSink)
    // ============================================================
    println!(
        "\n=== Phase 2: WLAN 802.11 TX ({} frames) ===",
        args.n_frames
    );
    let t_wlan = Instant::now();
    {
        let mut fg = Flowgraph::new();

        let mac = fg.add_block_dyn(
            wlan_mac.prepare(Box::new(([0x42u8; 6], [0x23u8; 6], [0xffu8; 6]))),
        );
        let mac_id: BlockId = mac;
        let encoder = fg.add_block_dyn(wlan_enc.prepare(Box::new("qpsk12".to_string())));
        let encoder_id: BlockId = encoder;
        let mapper = fg.add_block_dyn(wlan_map.prepare(Box::new(())));
        let ifft = fg.add_block_dyn(
            fft.prepare(Box::new((64_usize, true, true, Some((1.0f32 / 52.0).sqrt())))),
        );
        let prefix = fg.add_block_dyn(wlan_pfx.prepare(Box::new((100_usize, 100_usize))));
        let snk = fg.reuse_block(snk_block.clone());

        fg.connect_message(mac_id, "tx", encoder_id, "tx")?;
        fg.connect_dyn(encoder_id, "output", mapper, "input")?;
        fg.connect_dyn(mapper, "output", ifft, "input")?;
        fg.connect_dyn(ifft, "output", prefix, "input")?;
        fg.connect_dyn(prefix, "output", snk, "input")?;

        let (_fg_task, mut handle) = rt.start_sync(fg)?;

        let n_frames = args.n_frames;
        let frame_delay = args.frame_delay;

        rt.block_on(async move {
            for seq in 0..n_frames {
                Timer::after(Duration::from_secs_f64(frame_delay)).await;
                handle
                    .call(
                        mac_id,
                        "tx",
                        Pmt::Blob(format!("WLAN frame {seq}").as_bytes().to_vec()),
                    )
                    .await
                    .unwrap();
            }
            Timer::after(Duration::from_secs_f64(0.2)).await;
            handle.terminate_and_wait().await.unwrap();
        });
    }
    let wlan_elapsed = t_wlan.elapsed();
    println!("WLAN TX done in {:.3}s", wlan_elapsed.as_secs_f64());

    println!("\n=== TX PHY Swap Complete (NullSink reused) ===");
    println!("  ZigBee: {:.3}s", zigbee_elapsed.as_secs_f64());
    println!("  WLAN:   {:.3}s", wlan_elapsed.as_secs_f64());
    println!(
        "  Total:  {:.3}s",
        (zigbee_elapsed + wlan_elapsed).as_secs_f64()
    );

    Ok(())
}
