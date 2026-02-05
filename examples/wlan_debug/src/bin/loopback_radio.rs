use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::blocks::Apply;
use futuresdr::blocks::BlobToUdp;
use futuresdr::blocks::Combine;
use futuresdr::blocks::Delay;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FftDirection;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::Throttle;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::seify::Builder;
use futuresdr::futures::StreamExt;
use futuresdr::prelude::*;
use std::time::Duration;

use wlan::Decoder;
use wlan::Encoder;
use wlan::FrameEqualizer;
use wlan::Mac;
use wlan::Mapper;
use wlan::Mcs;
use wlan::MovingAverage;
use wlan::Prefix;
use wlan::SyncLong;
use wlan::SyncShort;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Seify/SoapySDR device args
    #[clap(short, long)]
    args: Option<String>,
    /// TX Gain
    #[clap(long, default_value_t = 80.0)]
    tx_gain: f64,
    /// RX Gain
    #[clap(long, default_value_t = 20.0)]
    rx_gain: f64,
    /// Sample Rate
    #[clap(short, long, default_value_t = 2e6)]
    sample_rate: f64,
    /// Frequency (Hz)
    #[clap(short, long, default_value_t = 2300e6)]
    frequency: f64,
}

const PAD_FRONT: usize = 10000;
const PAD_TAIL: usize = 10000;

fn main() -> Result<()> {
    let args = Args::parse();
    futuresdr::runtime::init();
    println!("Configuration: {args:?}");

    let mut fg = Flowgraph::new();

    // ========================================
    // Transmitter
    // ========================================
    let mac = Mac::new([0x42; 6], [0x23; 6], [0xff; 6]);
    let encoder: Encoder = Encoder::new(Mcs::Qpsk_1_2);
    connect!(fg, mac.tx | tx.encoder);
    let mapper: Mapper = Mapper::new();
    connect!(fg, encoder > mapper);
    let fft_tx: Fft = Fft::with_options(
        64,
        FftDirection::Inverse,
        true,
        Some((1.0f32 / 52.0).sqrt()),
    );
    connect!(fg, mapper > fft_tx);
    let prefix: Prefix = Prefix::new(PAD_FRONT, PAD_TAIL);
    connect!(fg, fft_tx > prefix);

    let throttle = Throttle::<Complex32>::new(10.0*args.sample_rate);

    // TX sink (ADALM-Pluto will use same device for TX and RX with loopback)
    let tx_sink = Builder::new(args.args.clone())?
        .frequency(args.frequency)
        .sample_rate(args.sample_rate)
        .gain(args.tx_gain)
        .build_sink()?;

    connect!(fg, prefix > throttle > inputs[0].tx_sink);

    // ========================================
    // Receiver
    // ========================================
    // RX source (hardware loopback on the Pluto)
    let rx_src = Builder::new(args.args)?
        .frequency(args.frequency)
        .sample_rate(args.sample_rate)
        .gain(args.rx_gain)
        .build_source()?;

    let delay = Delay::<Complex32>::new(16);
    connect!(fg, rx_src.outputs[0] > delay);

    let complex_to_mag_2 = Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr());
    let float_avg = MovingAverage::<f32>::new(64);
    connect!(fg, rx_src.outputs[0] > complex_to_mag_2 > float_avg);

    let mult_conj = Combine::<_, _, _, _>::new(|a: &Complex32, b: &Complex32| a * b.conj());
    let complex_avg = MovingAverage::<Complex32>::new(48);
    connect!(fg, rx_src.outputs[0] > in0.mult_conj > complex_avg);
    connect!(fg, delay > in1.mult_conj);

    let divide_mag = Combine::<_, _, _, _>::new(|a: &Complex32, b: &f32| a.norm() / b);
    connect!(fg, complex_avg > in0.divide_mag);
    connect!(fg, float_avg > in1.divide_mag);

    let sync_short: SyncShort = SyncShort::new();
    connect!(fg, delay > in_sig.sync_short);
    connect!(fg, complex_avg > in_abs.sync_short);
    connect!(fg, divide_mag > in_cor.sync_short);

    let sync_long: SyncLong = SyncLong::new();
    connect!(fg, sync_short > sync_long);

    let fft: Fft = Fft::new(64);
    connect!(fg, sync_long > fft);

    let frame_equalizer: FrameEqualizer = FrameEqualizer::new();
    connect!(fg, fft > frame_equalizer);

    let symbol_sink = WebsocketPmtSink::new(9002);
    let decoder = Decoder::new();
    connect!(fg, frame_equalizer > decoder);
    connect!(fg, frame_equalizer.symbols | symbol_sink);

    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    connect!(fg, decoder.rx_frames | message_pipe);
    let blob_to_udp = BlobToUdp::new("127.0.0.1:55555");
    connect!(fg, decoder.rx_frames | blob_to_udp);
    let blob_to_udp = BlobToUdp::new("127.0.0.1:55556");
    connect!(fg, decoder.rftap | blob_to_udp);
    let mac = mac.get()?.id;

    let rt = Runtime::new();
    let (_fg, mut handle) = rt.start_sync(fg)?;

    let mut seq = 0u64;
    rt.spawn_background(async move {
        loop {
            Timer::after(Duration::from_secs_f32(0.1)).await;
            handle
                .call(
                    mac,
                    "tx",
                    Pmt::Any(Box::new((
                        format!("FutureSDR {seq}xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").as_bytes().to_vec(),
                        Mcs::Qpsk_1_2,
                    ))),
                )
                .await
                .unwrap();
            seq += 1;
        }
    });

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
