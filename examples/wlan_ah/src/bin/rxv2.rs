//! rxv2 — 802.11ah receiver built from the per-step `v2` pipeline.
//!
//!   seify source
//!     → StfDetector      (stream Complex32 in, `frame` message out)
//!     → CfoCorrector     (`frame` in → `frame` out)
//!     → StoCorrector     (`frame` in → `frame` out)
//!     → ChannelEstimator (`frame` in → `frame` out)
//!     → SigDecoder       (`frame` in → `frame` out)
//!     → DataDemod        (`frame` in → u8 stream out, `wifi_start` tagged)
//!     → Decoder          (existing block, consumes the u8 stream)
//!
//! Diagnostic WebSocket sinks mirror `rx`:
//!   9012 — data symbols (VecCF32, from DataDemod)
//!   9013 — spectrogram  (f32 power bins, 256/FFT)
//!   9014 — STF detection metric landscape (f32 stream)
//!   9015 — raw IQ magnitude (f32 stream)
//!   9016 — channel estimate (VecCF32, 56 active SCs)
//!   9017 — SIG symbol 0 (VecCF32, 48 SCs)
//!   9018 — SIG symbol 1 (VecCF32, 48 SCs)
//!   9019 — StfDetector corr_mag snapshot (VecF32, 160)
//!   9020 — StfDetector sync_info           (VecF32, 6)
//!   9021 — LTF time-domain samples          (VecCF32, 128)

use clap::Parser;
use futuresdr::blocks::Apply;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FftDirection;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::WebsocketSinkBuilder;
use futuresdr::blocks::WebsocketSinkMode;
use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;

use wlan_ah::Decoder;
use wlan_ah::parse_channel;
use wlan_ah::v2::{
    ChannelEstimator, CfoCorrector, DataDemod, SigDecoder, StfDetector, StoCorrector,
};

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    #[clap(long)]
    antenna: Option<String>,
    #[clap(short, long)]
    args: Option<String>,
    #[clap(short, long, default_value_t = 10.0)]
    gain: f64,
    #[clap(short, long, default_value_t = 2e6)]
    sample_rate: f64,
    #[clap(short, long, value_parser = parse_channel, default_value = "6")]
    channel: f64,
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

    let stf: StfDetector = StfDetector::new();
    let cfo = CfoCorrector::new();
    let sto = StoCorrector::new();
    let ch = ChannelEstimator::new();
    let sig = SigDecoder::new();
    let data: DataDemod = DataDemod::new();
    let decoder = Decoder::new();

    // Main pipeline. StfDetector takes the stream input; all other hand-offs
    // between v2 blocks are messages on the `frame` port.
    connect!(fg,
        src.outputs[0] > stf;
        stf.frame | frame.cfo;
        cfo.frame | frame.sto;
        sto.frame | frame.ch;
        ch.frame | frame.sig;
        sig.frame | frame.data;
        data > decoder
    );
    let src_id: BlockId = src.into();

    // ── Diagnostic sinks (mirror rx) ───────────────────────────────────
    let symbol_sink = WebsocketPmtSink::new(9012);
    let h_est_sink = WebsocketPmtSink::new(9016);
    let preamble_sink = WebsocketPmtSink::new(9017);
    let preamble_sink2 = WebsocketPmtSink::new(9018);
    let sync_corr_ws = WebsocketPmtSink::new(9019);
    let sync_info_ws = WebsocketPmtSink::new(9020);
    let ltf_td_ws = WebsocketPmtSink::new(9021);
    connect!(fg,
        data.symbols           | r#in.symbol_sink;
        ch.channel_est         | r#in.h_est_sink;
        sig.preamble_symbols   | r#in.preamble_sink;
        sig.preamble_symbols2  | r#in.preamble_sink2;
        stf.corr_mag           | r#in.sync_corr_ws;
        stf.sync_info          | r#in.sync_info_ws;
        sto.ltf_td             | r#in.ltf_td_ws
    );

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
    fg.connect_dyn(src_id, "outputs[0]", fft_spec_id, "input")?;
    fg.connect_dyn(fft_spec_id, "output", fft_to_power_id, "input")?;
    fg.connect_dyn(fft_to_power_id, "output", spectrogram_ws_id, "input")?;

    // Raw IQ magnitude — ws://127.0.0.1:9015
    let src_mag = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm()));
    let src_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9015)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    let src_mag_id: BlockId = src_mag.into();
    let src_ws_id: BlockId = src_ws.into();
    fg.connect_dyn(src_id, "outputs[0]", src_mag_id, "input")?;
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
