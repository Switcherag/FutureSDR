use clap::Parser;
use futuresdr::blocks::Apply;
use futuresdr::blocks::Combine;
use futuresdr::blocks::Delay;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FftDirection;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::WebsocketSinkBuilder;
use futuresdr::blocks::WebsocketSinkMode;
use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;

use wlan_ah::Decoder;
use wlan_ah::FrameEqualizer;
use wlan_ah::MovingAverage;
use wlan_ah::SyncLong;
use wlan_ah::SyncShort;
use wlan_ah::parse_channel;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Antenna
    #[clap(long)]
    antenna: Option<String>,
    /// Seify Args
    #[clap(short, long)]
    args: Option<String>,
    /// Gain
    #[clap(short, long, default_value_t = 0.0)]
    gain: f64,
    /// Sample Rate
    /// Sample Rate
    #[clap(short, long, default_value_t = 2e6)]
    sample_rate: f64,
    /// WLAN Channel Number
    #[clap(short, long, value_parser = parse_channel, default_value = "6")]
    channel: f64,
    /// DC Offset
    #[clap(short, long, default_value_t = false)]
    dc_offset: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    let src = Builder::new(args.args)?
        .frequency(args.channel)
        .sample_rate(args.sample_rate)
        .gain(args.gain)
        .antenna(args.antenna)
        .build_source()?;

    connect!(fg, src);

    let (prev, output): (BlockId, _) = if args.dc_offset {
        let mut avg_real = 0.0;
        let mut avg_img = 0.0;
        let ratio = 1.0e-5;
        let dc = Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
            avg_real = ratio * (c.re - avg_real) + avg_real;
            avg_img = ratio * (c.im - avg_img) + avg_img;
            Complex32::new(c.re - avg_real, c.im - avg_img)
        });

        connect!(fg, src.outputs[0] > dc);
        (dc.into(), "output")
    } else {
        (src.into(), "outputs[0]")
    };

    let delay = fg.add_block(Delay::<Complex32>::new(16));
    fg.connect_dyn(prev, output, &delay, "input")?;

    let complex_to_mag_2 = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
    let float_avg = MovingAverage::<f32>::new(64);
    fg.connect_dyn(prev, output, &complex_to_mag_2, "input")?;
    connect!(fg, complex_to_mag_2 > float_avg);

    let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &Complex32| a * b.conj(),
    ));
    let complex_avg = MovingAverage::<Complex32>::new(48);
    fg.connect_dyn(prev, output, &mult_conj, "in0")?;
    connect!(fg, mult_conj > complex_avg;
                 delay > in1.mult_conj);

    let divide_mag = fg.add_block(
        Combine::<_, _, _, _>::new(|a: &Complex32, b: &f32| a.norm() / b)
    );
    connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);
    let divide_mag_id: BlockId = divide_mag.into();

    let sync_short: SyncShort = SyncShort::new();
    connect!(fg, delay > in_sig.sync_short;
                 complex_avg > in_abs.sync_short);
    fg.connect_dyn(divide_mag_id, "output", &sync_short, "in_cor")?;

    let sync_long: SyncLong = SyncLong::new();
    connect!(fg, sync_short > sync_long);

    // ── SyncLong diagnostics ───────────────────────────────────────────
    // Correlation landscape — ws://127.0.0.1:9019 (VecF32, 160 pts)
    let sync_corr_ws = WebsocketPmtSink::new(9019);
    // Peak indices + fine CFO — ws://127.0.0.1:9020
    //   [first_peak, second_peak, gap, fine_cfo, mag1, mag2]
    let sync_info_ws = WebsocketPmtSink::new(9020);
    // Time-domain LTF samples post-sync (pre-CFO-correction) — ws://127.0.0.1:9021
    let ltf_td_ws = WebsocketPmtSink::new(9021);
    connect!(fg, sync_long.corr_mag | r#in.sync_corr_ws;
                 sync_long.sync_info | r#in.sync_info_ws;
                 sync_long.ltf_td | r#in.ltf_td_ws);

    let fft: Fft = Fft::new(wlan_ah::FFT_SIZE);
    let frame_equalizer: FrameEqualizer = FrameEqualizer::new();
    let decoder = Decoder::new();
    let symbol_sink = WebsocketPmtSink::new(9012);
    let preamble_sink = WebsocketPmtSink::new(9017);
    let preamble_sink2 = WebsocketPmtSink::new(9018);
    let h_est_sink = WebsocketPmtSink::new(9016);
    connect!(fg, sync_long > fft > frame_equalizer > decoder;
        frame_equalizer.symbols | r#in.symbol_sink;
        frame_equalizer.preamble_symbols | r#in.preamble_sink;
        frame_equalizer.preamble_symbols2 | r#in.preamble_sink2;
        frame_equalizer.channel_est | r#in.h_est_sink);

    // ── Visualization sinks (mirror rx_debug) ──────────────────────────
    // Spectrogram — ws://127.0.0.1:9013
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
    fg.connect_dyn(prev, output, fft_spec_id, "input")?;
    fg.connect_dyn(fft_spec_id, "output", fft_to_power_id, "input")?;
    fg.connect_dyn(fft_to_power_id, "output", spectrogram_ws_id, "input")?;

    // Correlation — ws://127.0.0.1:9014
    // Notebook-style metric:  M[k] = |Σ_{k-143..k} x[j]·conj(x[j-16])|
    //                              ÷ mean(|x[j]|² for j in [k-80..k-41])
    // i.e. P is the raw 144-sample sum (like np.convolve(..., ones(144)))
    // and R is the pre-STF noise estimate (notebook norm_window = (-Ts, -Ts/2)).
    let nb_pow      = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm_sqr()));
    let nb_r_ma     = fg.add_block(MovingAverage::<f32>::new(40));       // Ts/2
    let nb_r_delay  = fg.add_block(Delay::<f32>::new(40));               // shift so R comes from [k-80..k-41]
    let nb_delay16  = fg.add_block(Delay::<Complex32>::new(16));         // Tu/4
    let nb_prod     = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &Complex32| a * b.conj(),
    ));
    let nb_p_ma     = fg.add_block(MovingAverage::<Complex32>::new(144)); // 2·Ts - Tu/4
    // MovingAverage returns the raw *sum* (not sum/N), so:
    //   |P| = |nb_p_ma|               (notebook's stf_corr)
    //   R   = nb_r_delay / 40         (mean over the 40-sample pre-STF window)
    // ⇒  M = |P| / R = |nb_p_ma| * 40 / nb_r_delay
    let nb_divide   = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &f32| a.norm() * 40.0 / b.max(1e-9),
    ));
    let cor_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9014)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    let nb_pow_id: BlockId      = nb_pow.into();
    let nb_r_ma_id: BlockId     = nb_r_ma.into();
    let nb_r_delay_id: BlockId  = nb_r_delay.into();
    let nb_delay16_id: BlockId  = nb_delay16.into();
    let nb_prod_id: BlockId     = nb_prod.into();
    let nb_p_ma_id: BlockId     = nb_p_ma.into();
    let nb_divide_id: BlockId   = nb_divide.into();
    let cor_ws_id: BlockId      = cor_ws.into();
    // R path: src → |·|² → MA(40) → Delay(40)
    fg.connect_dyn(prev, output, nb_pow_id, "input")?;
    fg.connect_dyn(nb_pow_id, "output", nb_r_ma_id, "input")?;
    fg.connect_dyn(nb_r_ma_id, "output", nb_r_delay_id, "input")?;
    // P path: src → Delay(16); (src, delayed) → conj-product → MA(144)
    fg.connect_dyn(prev, output, nb_delay16_id, "input")?;
    fg.connect_dyn(prev, output, nb_prod_id, "in0")?;
    fg.connect_dyn(nb_delay16_id, "output", nb_prod_id, "in1")?;
    fg.connect_dyn(nb_prod_id, "output", nb_p_ma_id, "input")?;
    // Divide and publish
    fg.connect_dyn(nb_p_ma_id, "output", nb_divide_id, "in0")?;
    fg.connect_dyn(nb_r_delay_id, "output", nb_divide_id, "in1")?;
    fg.connect_dyn(nb_divide_id, "output", cor_ws_id, "input")?;
    // Keep `divide_mag_id` available for sync_short's in_cor (untouched above).
    let _ = divide_mag_id;

    // Raw IQ magnitude — ws://127.0.0.1:9015
    let src_mag = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm()));
    let src_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9015)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    let src_mag_id: BlockId = src_mag.into();
    let src_ws_id: BlockId = src_ws.into();
    fg.connect_dyn(prev, output, src_mag_id, "input")?;
    fg.connect_dyn(src_mag_id, "output", src_ws_id, "input")?;

    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    let udp1 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55555");
    let udp2 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55556");
    connect!(fg, decoder.rx_frames | message_pipe;
                 decoder.rx_frames | udp1;
                 decoder.rftap | udp2);

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
