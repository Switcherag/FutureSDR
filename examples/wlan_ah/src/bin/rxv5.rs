//! rxv5 — 802.11ah receiver built from the streaming `v5` pipeline.
//!
//!   seify source → StreamingRx → Decoder
//!
//! One block replaces v2/v4's six-stage message pipeline. It buffers the
//! preamble (1,088 samples ≈ 0.27 ms) rather than a worst-case PPDU
//! (76,448 samples ≈ 19 ms), decodes SIG there, and demodulates each data
//! symbol as it arrives.

use clap::Parser;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;

use wlan_ah::Decoder;
use wlan_ah::parse_channel;
use wlan_ah::v5::StreamingRx;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Antenna
    #[clap(long)]
    antenna: Option<String>,
    /// Seify device args, e.g. "soapy=bladerf"
    #[clap(short, long)]
    args: Option<String>,
    /// Gain
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,
    /// Sample rate. The PHY constants (`FFT_SIZE` 128, `CP_LEN` 32) are sized
    /// for 2 MHz RF at 2x oversampling, so this must be 4 MSps.
    #[clap(short, long, default_value_t = 4e6)]
    sample_rate: f64,
    /// S1G channel number (34 = 919.0 MHz, 2 MHz)
    #[clap(short, long, value_parser = parse_channel, default_value = "34")]
    channel: f64,
    /// Log every decoded SIG field
    #[clap(long, default_value_t = false)]
    debug_sig: bool,
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

    let rx: StreamingRx = StreamingRx::new_with_debug_print(args.debug_sig);
    let decoder = Decoder::new();
    let symbol_sink = WebsocketPmtSink::new(9012);

    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    let udp1 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55555");
    let udp2 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55556");

    connect!(fg,
        src.outputs[0] > rx;
        rx > decoder;
        rx.symbols | r#in.symbol_sink;
        decoder.rx_frames | message_pipe;
        decoder.rx_frames | udp1;
        decoder.rftap | udp2
    );

    let (_fg, _handle) = rt.start_sync(fg)?;
    rt.block_on(async move {
        let mut n = 0usize;
        while let Some(x) = rx_frame.next().await {
            match x {
                Pmt::Blob(data) => {
                    n += 1;
                    println!("received frame {n} ({} bytes)", data.len());
                }
                _ => break,
            }
        }
    });

    Ok(())
}
