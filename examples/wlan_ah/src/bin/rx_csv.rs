//! rx_csv — run any 802.11ah receiver version and log every decoded frame to
//! CSV, for PER-versus-IFS analysis.
//!
//! Versions:
//!   v2      six-stage message pipeline, buffers a worst-case PPDU
//!   v4      v2 with the frame clones and per-work() input copy removed
//!   v5      streaming: buffers only the preamble, symbol-at-a-time decode
//!   v6      fork of examples/wlan's 11a blocks moved to the S1G standard
//!   stream  SyncShort -> SyncLongV2 -> Fft(128) -> FrameEqualizer
//!
//! One row per successfully decoded frame (the `Decoder` only emits frames
//! whose FCS checks, so every row is a success). PER per IFS step comes from
//! clustering `delta_ms` — the gap since the previous frame — in post.
//!
//!   cargo run --release -p wlan_ah --bin rx_csv -- \
//!       --rx v5 -a soapy=bladerf --channel 34 --gain 40 \
//!       --duration 60 --out csv/per_v5.csv

use std::io::{LineWriter, Write};
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use futuresdr::async_io::Timer;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::{Apply, Combine, Delay, Fft, MessagePipe};
use futuresdr::prelude::*;

use wlan_ah::{Decoder, FrameEqualizer, MovingAverage, SyncLongV2, SyncShort, parse_channel};
use wlan_ah::{FFT_SIZE, SYMBOL_LEN};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Version {
    V2,
    V4,
    V5,
    V6,
    Stream,
}

impl Version {
    fn as_str(&self) -> &'static str {
        match self {
            Version::V2 => "v2",
            Version::V4 => "v4",
            Version::V5 => "v5",
            Version::V6 => "v6",
            Version::Stream => "stream",
        }
    }
}

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Receiver version to run. Named `--rx` because clap's derive already
    /// owns `--version`.
    #[clap(long = "rx", value_enum, default_value = "v5")]
    rx_version: Version,
    /// CSV output path
    #[clap(long, default_value = "rx.csv")]
    out: String,
    /// Stop after this many seconds (0 = run until killed)
    #[clap(long, default_value_t = 0.0)]
    duration: f64,
    /// Antenna
    #[clap(long)]
    antenna: Option<String>,
    /// Seify device args, e.g. "soapy=bladerf"
    #[clap(short, long)]
    args: Option<String>,
    /// Gain
    #[clap(short, long, default_value_t = 40.0)]
    gain: f64,
    /// Sample rate. PHY constants (FFT_SIZE 128, CP_LEN 32) require 4 MSps.
    #[clap(short, long, default_value_t = 4e6)]
    sample_rate: f64,
    /// S1G channel number (34 = 919.0 MHz, 2 MHz)
    #[clap(short, long, value_parser = parse_channel, default_value = "34")]
    channel: f64,
    /// Max PSDU the v2/v4 detector sizes its cold start for (bytes).
    /// Smaller = shorter cold start. Ignored by v5 and stream.
    #[clap(long, default_value_t = 256)]
    max_psdu: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    let src = Builder::new(args.args.clone())?
        .frequency(args.channel)
        .sample_rate(args.sample_rate)
        .gain(args.gain)
        .antenna(args.antenna.clone())
        .build_source()?;
    let decoder = Decoder::new();
    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(1000);
    let message_pipe = MessagePipe::new(tx_frame);

    match args.rx_version {
        Version::V5 => {
            let rx: wlan_ah::v5::StreamingRx = wlan_ah::v5::StreamingRx::new();
            connect!(fg, src.outputs[0] > rx; rx > decoder;
                         decoder.rx_frames | message_pipe);
        }
        Version::V4 => {
            use wlan_ah::v4::{
                ChannelEstimator, CfoCorrector, DataDemod, SigDecoder, StfDetector, StoCorrector,
            };
            let stf: StfDetector = StfDetector::with_max_psdu(args.max_psdu);
            let cfo = CfoCorrector::new();
            let sto = StoCorrector::new();
            let ch = ChannelEstimator::new();
            let sig = SigDecoder::new();
            let data: DataDemod = DataDemod::new();
            connect!(fg,
                src.outputs[0] > stf;
                stf.frame | frame.cfo;
                cfo.frame | frame.sto;
                sto.frame | frame.ch;
                ch.frame  | frame.sig;
                sig.frame | frame.data;
                data > decoder;
                decoder.rx_frames | message_pipe
            );
        }
        Version::V2 => {
            use wlan_ah::v2::{
                ChannelEstimator, CfoCorrector, DataDemod, SigDecoder, StfDetector, StoCorrector,
            };
            let stf: StfDetector = StfDetector::with_max_psdu(args.max_psdu);
            let cfo = CfoCorrector::new();
            let sto = StoCorrector::new();
            let ch = ChannelEstimator::new();
            let sig = SigDecoder::new();
            let data: DataDemod = DataDemod::new();
            connect!(fg,
                src.outputs[0] > stf;
                stf.frame | frame.cfo;
                cfo.frame | frame.sto;
                sto.frame | frame.ch;
                ch.frame  | frame.sig;
                sig.frame | frame.data;
                data > decoder;
                decoder.rx_frames | message_pipe
            );
        }
        Version::V6 => {
            use wlan_ah::v6::{
                FrameEqualizer as BisEqualizer, STF_CORR_WIN, STF_DELAY, STF_POWER_WIN,
                SyncLong as BisSyncLong, SyncShort as BisSyncShort,
            };

            connect!(fg, src);
            let src_id: BlockId = src.into();

            let delay = fg.add_block(Delay::<Complex32>::new(STF_DELAY as isize));
            fg.connect_dyn(src_id, "outputs[0]", &delay, "input")?;

            let c2m = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
            let float_avg = MovingAverage::<f32>::new(STF_POWER_WIN);
            fg.connect_dyn(src_id, "outputs[0]", &c2m, "input")?;
            connect!(fg, c2m > float_avg);

            let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
                |a: &Complex32, b: &Complex32| a * b.conj(),
            ));
            let complex_avg = MovingAverage::<Complex32>::new(STF_CORR_WIN);
            fg.connect_dyn(src_id, "outputs[0]", &mult_conj, "in0")?;
            connect!(fg, mult_conj > complex_avg;
                         delay > in1.mult_conj);

            let divide_mag = fg.add_block(Combine::<_, _, _, _>::new(
                |a: &Complex32, b: &f32| a.norm() / b,
            ));
            connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);
            let divide_mag_id: BlockId = divide_mag.into();

            let sync_short: BisSyncShort = BisSyncShort::new();
            connect!(fg, delay > in_sig.sync_short;
                         complex_avg > in_abs.sync_short);
            fg.connect_dyn(divide_mag_id, "output", &sync_short, "in_cor")?;

            let sync_long: BisSyncLong = BisSyncLong::new();
            let fft: Fft = Fft::new(FFT_SIZE);
            let frame_eq: BisEqualizer = BisEqualizer::new();
            connect!(fg, sync_short > sync_long > fft > frame_eq > decoder;
                         decoder.rx_frames | message_pipe);
        }
        Version::Stream => {
            // Sizing follows rx_debug, not rx_v2 — the latter still carries
            // the 2 MSps / 64-FFT values (16 / 64 / 48).
            const STF_DELAY: usize = FFT_SIZE / 4; // Tu/4 = 32 @ 4 MSps
            const STF_CORR_WIN: usize = 2 * SYMBOL_LEN - STF_DELAY; // 288
            const STF_POWER_WIN: usize = SYMBOL_LEN / 2; // 80

            connect!(fg, src);
            let src_id: BlockId = src.into();

            let delay = fg.add_block(Delay::<Complex32>::new(STF_DELAY as isize));
            fg.connect_dyn(src_id, "outputs[0]", &delay, "input")?;

            let c2m = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
            let float_avg = MovingAverage::<f32>::new(STF_POWER_WIN);
            fg.connect_dyn(src_id, "outputs[0]", &c2m, "input")?;
            connect!(fg, c2m > float_avg);

            let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
                |a: &Complex32, b: &Complex32| a * b.conj(),
            ));
            let complex_avg = MovingAverage::<Complex32>::new(STF_CORR_WIN);
            fg.connect_dyn(src_id, "outputs[0]", &mult_conj, "in0")?;
            connect!(fg, mult_conj > complex_avg;
                         delay > in1.mult_conj);

            let divide_mag = fg.add_block(Combine::<_, _, _, _>::new(
                |a: &Complex32, b: &f32| a.norm() / b,
            ));
            connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);
            let divide_mag_id: BlockId = divide_mag.into();

            let sync_short: SyncShort = SyncShort::new();
            connect!(fg, delay > in_sig.sync_short;
                         complex_avg > in_abs.sync_short);
            fg.connect_dyn(divide_mag_id, "output", &sync_short, "in_cor")?;

            let sync_long: SyncLongV2 = SyncLongV2::new();
            let fft: Fft = Fft::new(FFT_SIZE);
            let frame_eq: FrameEqualizer = FrameEqualizer::new();
            connect!(fg, sync_short > sync_long > fft > frame_eq > decoder;
                         decoder.rx_frames | message_pipe);
        }
    }

    let path = args.out.clone();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).ok();
        }
    }
    let mut csv = LineWriter::new(std::fs::File::create(&path)?);
    writeln!(csv, "idx,t_rel_s,delta_ms,len,version,payload_prefix")?;

    let version = args.rx_version.as_str();
    if args.duration > 0.0 {
        let d = args.duration;
        rt.spawn_background(async move {
            Timer::after(Duration::from_secs_f64(d)).await;
            std::process::exit(0);
        });
    }

    let (_fg, _handle) = rt.start_sync(fg)?;
    let t0 = Instant::now();
    rt.block_on(async move {
        let mut idx = 0usize;
        let mut last: Option<f64> = None;
        while let Some(x) = rx_frame.next().await {
            match x {
                Pmt::Blob(data) => {
                    let t = t0.elapsed().as_secs_f64();
                    let delta_ms = match last {
                        Some(prev) => (t - prev) * 1e3,
                        None => f64::NAN,
                    };
                    last = Some(t);
                    idx += 1;
                    let prefix: String = data
                        .iter()
                        .take(16)
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    // Rows are flushed per line, so a run cut short by
                    // `timeout` or --duration still leaves a complete CSV.
                    let _ = writeln!(
                        csv,
                        "{idx},{t:.9},{delta_ms:.6},{},{version},{prefix}",
                        data.len()
                    );
                    if idx % 50 == 0 {
                        println!("[{version}] {idx} frames, t={t:.2}s");
                    }
                }
                _ => break,
            }
        }
        println!("[{version}] done: {idx} frames -> {path}");
    });

    Ok(())
}
