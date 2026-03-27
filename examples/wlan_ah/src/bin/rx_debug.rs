use clap::Parser;
use futuresdr::blocks::Apply;
use futuresdr::blocks::Combine;
use futuresdr::blocks::Delay;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FftDirection;
use futuresdr::blocks::FileSource;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::WebsocketSinkBuilder;
use futuresdr::blocks::WebsocketSinkMode;
use futuresdr::prelude::*;

use wlan_ah::Decoder;
use wlan_ah::FrameEqualizer;
use wlan_ah::MovingAverage;
use wlan_ah::SyncLong;
use wlan_ah::SyncShort;
use wlan_ah::{FFT_SIZE, SYMBOL_LEN};

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// DC Offset
    #[clap(short, long, default_value_t = false)]
    dc_offset: bool,
    /// Input file path
    #[clap(short = 'i', long, default_value = "2024-12-22-21-19-29_baby-monitor_4Msps.sigmf-data")]
    file: String,
    /// Throttle rate in samples/sec (use high value like 1e9 for batch)
    #[clap(short, long, default_value_t = 1e3)]
    rate: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    // ── Source ──────────────────────────────────────────────────────────
    let source = FileSource::<Complex<i16>>::new(
        &args.file,
        true,
    );
    let throttle = futuresdr::blocks::Throttle::<Complex<i16>>::new(args.rate);
    let convert = Apply::<_, _, _>::new(|c: &Complex<i16>| {
        Complex32::new(
            c.re as f32 * (1.0 / 32768.0),
            c.im as f32 * (1.0 / 32768.0),
        )
    });
    connect!(fg, source > throttle > convert);
    let convert_id: BlockId = convert.into();

    // ── Optional DC offset removal ─────────────────────────────────────
    let (prev, output): (BlockId, &str) = if args.dc_offset {
        let mut avg_real = 0.0;
        let mut avg_img = 0.0;
        let ratio = 1.0e-5;
        let dc = fg.add_block(Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
            avg_real = ratio * (c.re - avg_real) + avg_real;
            avg_img = ratio * (c.im - avg_img) + avg_img;
            Complex32::new(c.re - avg_real, c.im - avg_img)
        }));
        let dc_id: BlockId = dc.into();
        fg.connect_dyn(convert_id, "output", dc_id, "input")?;
        (dc_id, "output")
    } else {
        (convert_id, "output")
    };

    // ── Schmidl-Cox short preamble detector ────────────────────────────
    // M[n] = |Σ x[n+k]·x*[n+k-16]| / Σ|x[n+k]|²
    // Schmidl-Cox: delay = Tu/4 = FFT_SIZE/4
    let delay = fg.add_block(Delay::<Complex32>::new((FFT_SIZE / 4) as isize));
    fg.connect_dyn(prev, output, &delay, "input")?;

    let complex_to_mag_2 = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
    let float_avg = MovingAverage::<f32>::new(SYMBOL_LEN);
    fg.connect_dyn(prev, output, &complex_to_mag_2, "input")?;
    connect!(fg, complex_to_mag_2 > float_avg);

    let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &Complex32| a * b.conj(),
    ));
    let complex_avg = MovingAverage::<Complex32>::new(SYMBOL_LEN - FFT_SIZE / 4);
    fg.connect_dyn(prev, output, &mult_conj, "in0")?;
    connect!(fg, mult_conj > complex_avg;
                 delay > in1.mult_conj);

    let divide_mag = fg.add_block(
        Combine::<_, _, _, _>::new(|a: &Complex32, b: &f32| a.norm() / b),
    );
    connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);
    let divide_mag_id: BlockId = divide_mag.into();

    // ── Sync & decode ──────────────────────────────────────────────────
    let sync_short: SyncShort = SyncShort::new();
    connect!(fg, delay > in_sig.sync_short;
                 complex_avg > in_abs.sync_short);
    fg.connect_dyn(divide_mag_id, "output", &sync_short, "in_cor")?;

    let sync_long: SyncLong = SyncLong::new();
    connect!(fg, sync_short > sync_long);

    let fft: Fft = Fft::new(FFT_SIZE);
    let frame_equalizer: FrameEqualizer = FrameEqualizer::new();
    let decoder = Decoder::new();
    connect!(fg, sync_long > fft > frame_equalizer > decoder);

    // ── Frame output ───────────────────────────────────────────────────
    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    let udp1 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55555");
    let udp2 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55556");
    connect!(fg, decoder.rx_frames | message_pipe;
                 decoder.rx_frames | udp1;
                 decoder.rftap | udp2);

    // ── Visualization sinks ────────────────────────────────────────────
    // Spectrogram (waterfall) — ws://127.0.0.1:9013
    let fft_spec_block: Fft = Fft::with_options(2048 / 8, FftDirection::Forward, true, None);
    let fft_spec = fg.add_block(fft_spec_block);
    let fft_to_power = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm_sqr()));
    let spectrogram_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9013)
            .mode(WebsocketSinkMode::FixedDropping(2048 / 8))
            .build(),
    );
    let fft_spec_id: BlockId = fft_spec.into();
    let fft_to_power_id: BlockId = fft_to_power.into();
    let spectrogram_ws_id: BlockId = spectrogram_ws.into();
    fg.connect_dyn(convert_id, "output", fft_spec_id, "input")?;
    fg.connect_dyn(fft_spec_id, "output", fft_to_power_id, "input")?;
    fg.connect_dyn(fft_to_power_id, "output", spectrogram_ws_id, "input")?;

    // Constellation symbols — ws://127.0.0.1:9012
    let symbol_sink = WebsocketPmtSink::new(9012);
    connect!(fg, frame_equalizer.symbols | r#in.symbol_sink);

    // Channel estimate — ws://127.0.0.1:9016
    let h_est_sink = WebsocketPmtSink::new(9016);
    connect!(fg, frame_equalizer.channel_est | r#in.h_est_sink);

    // Correlation metric (time sink) — ws://127.0.0.1:9014
    // FixedDropping(256) matches spectrogram chunk size so they scroll together.
    let cor_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9014)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    fg.connect_dyn(divide_mag_id, "output", cor_ws, "input")?;

    // Raw source magnitude (time sink) — ws://127.0.0.1:9015
    let src_mag = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm()));
    let src_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9015)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    let src_mag_id: BlockId = src_mag.into();
    let src_ws_id: BlockId = src_ws.into();
    fg.connect_dyn(convert_id, "output", src_mag_id, "input")?;
    fg.connect_dyn(src_mag_id, "output", src_ws_id, "input")?;

    // ── Run ────────────────────────────────────────────────────────────
    let (_fg, _handle) = rt.start_sync(fg)?;
    rt.block_on(async move {
        while let Some(x) = rx_frame.next().await {
            match x {
                Pmt::Blob(data) => {
                    println!("received frame ({:?} bytes)", data.len());
                }
                _ => break,
            }
        }
    });

    Ok(())
}
