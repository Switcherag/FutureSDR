// Dynamic PHY swap example — demonstrates runtime PHY switching.
//
// Builds a WLAN 802.11 RX flowgraph, runs it for 1 second, then
// terminates and switches to a Zigbee 802.15.4 RX flowgraph.
//
// SeifySource is imported statically from the library.
// All other blocks are loaded as dynamic plugins at runtime.

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io;
use futuresdr::blocks::seify::Builder;
use futuresdr::runtime::{Flowgraph, Runtime};
use plugin_host::LoadedPlugin;
use std::time::Duration;

#[derive(Parser, Debug)]
struct Args {
    /// Seify device args
    #[clap(short, long, default_value = "")]
    args: String,

    /// Gain
    #[clap(short, long, default_value_t = 30.0)]
    gain: f64,

    /// WLAN frequency (Hz)
    #[clap(long, default_value_t = 2.437e9)]
    wlan_freq: f64,

    /// WLAN sample rate (Hz)
    #[clap(long, default_value_t = 20e6)]
    wlan_rate: f64,

    /// Zigbee channel (11-26)
    #[clap(long, default_value_t = 26)]
    zigbee_channel: u32,

    /// Zigbee sample rate (Hz)
    #[clap(long, default_value_t = 4e6)]
    zigbee_rate: f64,

    /// Directory containing plugin .so files
    #[clap(long, default_value = ".")]
    plugin_dir: String,

    /// Duration of WLAN phase in seconds
    #[clap(long, default_value_t = 1)]
    wlan_duration: u64,
}

fn zigbee_channel_to_freq(chan: u32) -> f64 {
    (2400.0 + 5.0 * (chan as f64 - 10.0)) * 1e6
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let dir = &args.plugin_dir;

    // ============================================================
    // Load ALL plugins at startup
    // ============================================================

    // WLAN plugins
    let delay_complex = unsafe { LoadedPlugin::load(&format!("{dir}/libdelay_complex_plugin.so")) };
    let fft_complex = unsafe { LoadedPlugin::load(&format!("{dir}/libfft_complex_plugin.so")) };
    let complex_to_mag2 =
        unsafe { LoadedPlugin::load(&format!("{dir}/libcomplex_to_mag2_plugin.so")) };
    let moving_avg_f32 =
        unsafe { LoadedPlugin::load(&format!("{dir}/libmoving_average_f32_plugin.so")) };
    let moving_avg_complex =
        unsafe { LoadedPlugin::load(&format!("{dir}/libmoving_average_complex_plugin.so")) };
    let mult_conj = unsafe { LoadedPlugin::load(&format!("{dir}/libmult_conj_plugin.so")) };
    let divide_mag = unsafe { LoadedPlugin::load(&format!("{dir}/libdivide_mag_plugin.so")) };
    let sync_short =
        unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_sync_short_plugin.so")) };
    let sync_long = unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_sync_long_plugin.so")) };
    let frame_eq =
        unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_frame_equalizer_plugin.so")) };
    let wlan_decoder =
        unsafe { LoadedPlugin::load(&format!("{dir}/libwlan_decoder_plugin.so")) };
    let blob_to_udp =
        unsafe { LoadedPlugin::load(&format!("{dir}/libblob_to_udp_plugin.so")) };

    // Zigbee plugins
    let zigbee_demod =
        unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_demod_plugin.so")) };
    let clock_recovery =
        unsafe { LoadedPlugin::load(&format!("{dir}/libclock_recovery_mm_plugin.so")) };
    let zigbee_decoder =
        unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_decoder_plugin.so")) };
    let zigbee_mac = unsafe { LoadedPlugin::load(&format!("{dir}/libzigbee_mac_plugin.so")) };
    let null_sink = unsafe { LoadedPlugin::load(&format!("{dir}/libnull_sink_plugin.so")) };

    println!("All plugins loaded successfully");

    let rt = Runtime::new();

    // ============================================================
    // Phase 1: WLAN RX flowgraph
    // ============================================================
    println!("\n=== Starting WLAN RX (freq={} Hz) ===", args.wlan_freq);
    {
        let mut fg = Flowgraph::new();

        // SeifySource (static)
        let src = Builder::new(&args.args)?
            .frequency(args.wlan_freq)
            .sample_rate(args.wlan_rate)
            .gain(args.gain)
            .build_source()?;
        let src = fg.add_block(src);
        let src_id = src.get()?.id;

        // Delay(16) for signal path
        let delay = fg.add_block_dyn(delay_complex.prepare(Box::new(16_isize)));

        // Complex to Mag^2
        let c2m = fg.add_block_dyn(complex_to_mag2.prepare(Box::new(())));

        // Moving average f32(64)
        let ma_f32 = fg.add_block_dyn(moving_avg_f32.prepare(Box::new(64_usize)));

        // Delay(16) for conjugate path
        let delay2 = fg.add_block_dyn(delay_complex.prepare(Box::new(16_isize)));

        // MultConj (a * conj(b))
        let mc = fg.add_block_dyn(mult_conj.prepare(Box::new(())));

        // Moving average complex(48)
        let ma_complex = fg.add_block_dyn(moving_avg_complex.prepare(Box::new(48_usize)));

        // DivideMag (complex.norm() / f32)
        let dm = fg.add_block_dyn(divide_mag.prepare(Box::new(())));

        // SyncShort (3 inputs: in_sig, in_abs, in_cor)
        let ss = fg.add_block_dyn(sync_short.prepare(Box::new(())));

        // SyncLong
        let sl = fg.add_block_dyn(sync_long.prepare(Box::new(())));

        // FFT(64)
        let fft = fg.add_block_dyn(fft_complex.prepare(Box::new(64_usize)));

        // FrameEqualizer
        let eq = fg.add_block_dyn(frame_eq.prepare(Box::new(())));

        // WLAN Decoder
        let dec = fg.add_block_dyn(wlan_decoder.prepare(Box::new(())));

        // BlobToUdp for rx_frames (:55555)
        let udp_rx =
            fg.add_block_dyn(blob_to_udp.prepare(Box::new("127.0.0.1:55555".to_string())));

        // BlobToUdp for rftap (:55556)
        let udp_rftap =
            fg.add_block_dyn(blob_to_udp.prepare(Box::new("127.0.0.1:55556".to_string())));

        // === Stream connections ===
        // src -> delay (signal path)
        fg.connect_dyn(src_id, "outputs[0]", delay, "input")?;

        // src -> complex_to_mag2 -> moving_avg_f32(64) (power path)
        fg.connect_dyn(src_id, "outputs[0]", c2m, "input")?;
        fg.connect_dyn(c2m, "output", ma_f32, "input")?;

        // src -> delay2(16) -> mult_conj.in1 (delayed conjugate path)
        fg.connect_dyn(src_id, "outputs[0]", delay2, "input")?;
        fg.connect_dyn(delay2, "output", mc, "in1")?;

        // src -> mult_conj.in0 (direct)
        fg.connect_dyn(src_id, "outputs[0]", mc, "in0")?;

        // mult_conj -> moving_avg_complex(48) -> divide_mag.in0
        fg.connect_dyn(mc, "output", ma_complex, "input")?;
        fg.connect_dyn(ma_complex, "output", dm, "in0")?;

        // moving_avg_f32 -> divide_mag.in1
        fg.connect_dyn(ma_f32, "output", dm, "in1")?;

        // SyncShort inputs: in_sig from delay, in_abs from ma_complex, in_cor from divide_mag
        fg.connect_dyn(delay, "output", ss, "in_sig")?;
        fg.connect_dyn(ma_complex, "output", ss, "in_abs")?;
        fg.connect_dyn(dm, "output", ss, "in_cor")?;

        // SyncShort -> SyncLong -> FFT -> FrameEqualizer -> Decoder
        fg.connect_dyn(ss, "output", sl, "input")?;
        fg.connect_dyn(sl, "output", fft, "input")?;
        fg.connect_dyn(fft, "output", eq, "input")?;
        fg.connect_dyn(eq, "output", dec, "input")?;

        // Message connections
        fg.connect_message(dec, "rx_frames", udp_rx, "in")?;
        fg.connect_message(dec, "rftap", udp_rftap, "in")?;

        let (_res, mut handle) = rt.start_sync(fg)?;

        println!(
            "WLAN RX running, waiting {} second(s)...",
            args.wlan_duration
        );
        std::thread::sleep(Duration::from_secs(args.wlan_duration));

        println!("Terminating WLAN flowgraph...");
        async_io::block_on(handle.terminate_and_wait())?;
        println!("WLAN flowgraph terminated.");
    }

    // ============================================================
    // Phase 2: Zigbee RX flowgraph
    // ============================================================
    let zigbee_freq = zigbee_channel_to_freq(args.zigbee_channel);
    println!(
        "\n=== Starting Zigbee RX (channel={}, freq={} Hz) ===",
        args.zigbee_channel, zigbee_freq
    );
    {
        let mut fg = Flowgraph::new();

        // SeifySource (static)
        let src = Builder::new(&args.args)?
            .frequency(zigbee_freq)
            .sample_rate(args.zigbee_rate)
            .gain(args.gain)
            .build_source()?;
        let src = fg.add_block(src);
        let src_id = src.get()?.id;

        // ZigbeeDemod (alpha=0.00016)
        let demod = fg.add_block_dyn(zigbee_demod.prepare(Box::new(0.00016_f32)));

        // ClockRecoveryMm (omega=2.0, gain_omega=0.000225, mu=0.5, gain_mu=0.03, omega_rel=0.0002)
        let cr = fg.add_block_dyn(
            clock_recovery.prepare(Box::new((2.0_f32, 0.000225_f32, 0.5_f32, 0.03_f32, 0.0002_f32))),
        );

        // ZigbeeDecoder (threshold=6)
        let dec = fg.add_block_dyn(zigbee_decoder.prepare(Box::new(6_u32)));

        // Zigbee Mac
        let mac = fg.add_block_dyn(zigbee_mac.prepare(Box::new(())));

        // NullSink for Mac's u8 output stream
        let ns = fg.add_block_dyn(null_sink.prepare(Box::new(())));

        // BlobToUdp for rftap (:55555)
        let udp_rftap =
            fg.add_block_dyn(blob_to_udp.prepare(Box::new("127.0.0.1:55555".to_string())));

        // === Stream connections ===
        fg.connect_dyn(src_id, "outputs[0]", demod, "input")?;
        fg.connect_dyn(demod, "output", cr, "input")?;
        fg.connect_dyn(cr, "output", dec, "input")?;

        // Mac u8 output -> null_sink
        fg.connect_dyn(mac, "output", ns, "input")?;

        // === Message connections ===
        // ZigbeeDecoder "out" -> Mac "rx"
        fg.connect_message(dec, "out", mac, "rx")?;

        // Mac "rftap" -> BlobToUdp
        fg.connect_message(mac, "rftap", udp_rftap, "in")?;

        let (_res, mut handle) = rt.start_sync(fg)?;

        println!("Zigbee RX running. Press Ctrl+C to exit.");
        std::thread::sleep(Duration::from_secs(u64::MAX));

        async_io::block_on(handle.terminate_and_wait())?;
    }

    Ok(())
}
