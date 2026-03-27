use clap::Parser;
use futuresdr::blocks::Apply;
use futuresdr::blocks::Combine;
use futuresdr::blocks::Delay;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FirBuilder;
use futuresdr::blocks::FileSource;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::WebsocketSink;
use futuresdr::blocks::WebsocketSinkMode;
use futuresdr::blocks::XlatingFir;
use futuresdr::prelude::*;

use hallow::Decoder;
use hallow::FrameEqualizer;
use hallow::MovingAverage;
use hallow::SyncLong;
use hallow::SyncShort;
use hallow::parse_channel;
use hallow::FFT_SIZE;
use hallow::GI_SAMPLES;
use hallow::N_DATA_SC;
use hallow::SYMBOL_SAMPLES;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Input file (SigMF .sigmf-data, cf32_le format). If not provided, uses SDR hardware.
    #[clap(short, long)]
    file: Option<String>,
    /// Antenna
    #[clap(long)]
    antenna: Option<String>,
    /// Seify Args
    #[clap(short, long)]
    args: Option<String>,
    /// Gain
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,
    /// Sample Rate of input (10e6 for broadband captures, 1e6 for channel-filtered)
    #[clap(short, long, default_value_t = 1e6)]
    sample_rate: f64,
    /// HaLow Channel Number (default: 43 = 915.5 MHz)
    #[clap(short, long, value_parser = parse_channel, default_value = "43")]
    channel: f64,
    /// Center frequency of the capture file (for frequency offset calculation)
    #[clap(long)]
    capture_center_freq: Option<f64>,
    /// DC Offset removal
    #[clap(short, long, default_value_t = false)]
    dc_offset: bool,
    /// Decimation factor (e.g. 10 to go from 10 MHz to 1 MHz)
    #[clap(long, default_value_t = 1)]
    decimate: usize,
    /// rftap UDP destination for Wireshark (default: 127.0.0.1:52150)
    #[clap(long, default_value = "127.0.0.1:52150")]
    rftap_dest: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("=== HaLow 802.11ah Debug Receiver ===");
    println!("Configuration: {args:?}");
    println!();
    println!("Wireshark setup:");
    println!("  1. sudo ip link set lo up  (if loopback is down)");
    println!("  2. Start Wireshark on loopback interface (lo)");
    println!("  3. Capture filter: udp port {}", args.rftap_dest.split(':').last().unwrap_or("52150"));
    println!("  4. Decode As -> rftap (UDP port {})  OR", args.rftap_dest.split(':').last().unwrap_or("52150"));
    println!("     Edit > Preferences > Protocols > DLT_USER > Encapsulation table:");
    println!("     User 0 (DLT=147) -> rftap");
    println!("  5. rftap will encapsulate IEEE 802.11 frames (DLT 105)");
    println!();
    println!("Alternative: use udpdump extcap pipe:");
    println!("  wireshark -k -i udpdump -o 'udpdump.port:{}' -o 'udpdump.payload_type:rftap'",
        args.rftap_dest.split(':').last().unwrap_or("52150"));
    println!();
    println!("Raw frames also sent to UDP 127.0.0.1:55555");
    println!("rftap frames also sent to UDP {}", args.rftap_dest);
    println!("=============================================");
    println!();

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    // === Source: file or SDR hardware ===
    let (prev, output): (BlockId, &str) = if let Some(ref file_path) = args.file {
        let src = FileSource::<Complex32>::new(file_path, false);
        let src_id = fg.add_block(src);
        (src_id.into(), "output")
    } else {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use futuresdr::blocks::seify::Builder;
            let src = Builder::new(args.args.clone())?
                .frequency(args.channel)
                .sample_rate(args.sample_rate)
                .gain(args.gain)
                .antenna(args.antenna.clone())
                .build_source()?;
            connect!(fg, src);
            (src.into(), "outputs[0]")
        }
        #[cfg(target_arch = "wasm32")]
        {
            panic!("SDR hardware not supported on WASM, use --file");
        }
    };

    // === Optional: frequency shift + decimation for broadband captures ===
    let (prev, output): (BlockId, &str) = if let Some(center_freq) = args.capture_center_freq {
        let freq_offset = (args.channel - center_freq) as f32;
        if args.decimate > 1 {
            let xlating: XlatingFir = XlatingFir::new(args.decimate, freq_offset, args.sample_rate as f32);
            let xlating_id: BlockId = fg.add_block(xlating).into();
            fg.connect_dyn(prev, output, &xlating_id, "input")?;
            (xlating_id, "output")
        } else {
            let omega = 2.0 * std::f64::consts::PI * freq_offset as f64 / args.sample_rate;
            let mut phase = 0.0f64;
            let shift = Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
                let rot = Complex32::from_polar(1.0, phase as f32);
                phase += omega;
                if phase > std::f64::consts::PI {
                    phase -= 2.0 * std::f64::consts::PI;
                }
                *c * rot
            });
            let shift_id: BlockId = fg.add_block(shift).into();
            fg.connect_dyn(prev, output, &shift_id, "input")?;
            (shift_id, "output")
        }
    } else if args.decimate > 1 {
        let decimator = FirBuilder::decimating::<Complex32, Complex32, f32>(args.decimate);
        let dec_id: BlockId = fg.add_block(decimator).into();
        fg.connect_dyn(prev, output, &dec_id, "input")?;
        (dec_id, "output")
    } else {
        (prev, output)
    };

    // === Optional: DC offset removal ===
    let (prev, output): (BlockId, &str) = if args.dc_offset {
        let mut avg_real = 0.0;
        let mut avg_img = 0.0;
        let ratio = 1.0e-5;
        let dc = Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
            avg_real = ratio * (c.re - avg_real) + avg_real;
            avg_img = ratio * (c.im - avg_img) + avg_img;
            Complex32::new(c.re - avg_real, c.im - avg_img)
        });
        let dc_id: BlockId = fg.add_block(dc).into();
        fg.connect_dyn(prev, output, &dc_id, "input")?;
        (dc_id, "output")
    } else {
        (prev, output)
    };

    // === Signal processing chain ===

    let delay = fg.add_block(Delay::<Complex32>::new(GI_SAMPLES as isize));
    fg.connect_dyn(prev, output, &delay, "input")?;

    let complex_to_mag_2 = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
    let float_avg = MovingAverage::<f32>::new(FFT_SIZE);
    fg.connect_dyn(prev, output, &complex_to_mag_2, "input")?;
    connect!(fg, complex_to_mag_2 > float_avg);

    let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &Complex32| a * b.conj(),
    ));
    let complex_avg = MovingAverage::<Complex32>::new(N_DATA_SC);
    fg.connect_dyn(prev, output, &mult_conj, "in0")?;
    connect!(fg, mult_conj > complex_avg;
                 delay > in1.mult_conj);

    let divide_mag = Combine::<_, _, _, _>::new(|a: &Complex32, b: &f32| a.norm() / b);
    connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);

    // === Scope: correlation metric (port 9004) ===
    let cor_scope = WebsocketSink::<f32>::new(9004, WebsocketSinkMode::FixedDropping(1024));
    connect!(fg, divide_mag > cor_scope);

    // === Sync Short ===
    let sync_short: SyncShort = SyncShort::new();
    connect!(fg, delay > in_sig.sync_short;
                 complex_avg > in_abs.sync_short;
                 divide_mag > in_cor.sync_short);

    // === Scope: post-sync short time domain (port 9001) ===
    let sync_scope = WebsocketSink::<Complex32>::new(9001, WebsocketSinkMode::FixedDropping(1024));
    connect!(fg, sync_short > sync_scope);

    // === Sync Long ===
    let sync_long: SyncLong = SyncLong::new();
    connect!(fg, sync_short > sync_long);

    // === FFT (32-point for 1 MHz HaLow) ===
    let fft: Fft = Fft::new(FFT_SIZE);

    // === Scope: post-FFT waterfall (port 9003) ===
    let fft_scope =
        WebsocketSink::<Complex32>::new(9003, WebsocketSinkMode::FixedDropping(FFT_SIZE));
    connect!(fg, sync_long > fft > fft_scope);

    // === Scope: power spectrum |FFT|² (port 9005) ===
    let fft_power = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm_sqr()));
    let spectrum_scope =
        WebsocketSink::<f32>::new(9005, WebsocketSinkMode::FixedDropping(FFT_SIZE));
    connect!(fg, fft > fft_power > spectrum_scope);

    // === Scope: eye diagram (port 9006) ===
    let symbol_len = SYMBOL_SAMPLES;
    let mut eye_count = 0usize;
    let eye_mapper = fg.add_block(Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
        let t = (eye_count % symbol_len) as f32 / symbol_len as f32;
        let x = t * 2.0 - 1.0;
        eye_count += 1;
        Complex32::new(x, c.re)
    }));
    let eye_scope =
        WebsocketSink::<Complex32>::new(9006, WebsocketSinkMode::FixedDropping(SYMBOL_SAMPLES * 32));
    fg.connect_dyn(prev, output, &eye_mapper, "input")?;
    connect!(fg, eye_mapper > eye_scope);

    // === Frame Equalizer ===
    let frame_equalizer: FrameEqualizer = FrameEqualizer::new();
    connect!(fg, fft > frame_equalizer);

    // === Scope: constellation (port 9002) ===
    let symbol_sink = WebsocketPmtSink::new(9002);
    connect!(fg, frame_equalizer.symbols | r#in.symbol_sink);

    // === Decoder ===
    let decoder = Decoder::new();
    connect!(fg, frame_equalizer > decoder);

    // === Output: rftap to Wireshark + raw frames + console ===
    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    let udp_raw = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55555");
    let udp_rftap = futuresdr::blocks::BlobToUdp::new(args.rftap_dest.clone());
    connect!(fg, decoder.rx_frames | message_pipe;
                 decoder.rx_frames | udp_raw;
                 decoder.rftap | udp_rftap);

    let (_fg, _handle) = rt.start_sync(fg)?;

    println!("Receiver running. Waiting for HaLow frames...");
    println!("rftap -> UDP {}", args.rftap_dest);
    println!();

    let mut frame_count = 0u64;
    rt.block_on(async move {
        while let Some(x) = rx_frame.next().await {
            match x {
                Pmt::Blob(data) => {
                    frame_count += 1;
                    println!(
                        "[Frame #{frame_count}] received HaLow frame ({} bytes): {:02x?}",
                        data.len(),
                        &data[..std::cmp::min(32, data.len())]
                    );
                }
                _ => break,
            }
        }
    });

    Ok(())
}
