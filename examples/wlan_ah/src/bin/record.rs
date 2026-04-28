use std::fs;
use std::path::Path;

use clap::Parser;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::Head;
use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Output IQ file path (cf32_le)
    #[clap(short, long, default_value = "bin/wlan_ah_capture_4Msps.cf32")]
    output: String,
    /// Antenna name
    #[clap(long)]
    antenna: Option<String>,
    /// Seify args string
    #[clap(short, long)]
    args: Option<String>,
    /// RF gain
    #[clap(short, long, default_value_t = 0.0)]
    gain: f64,
    /// Sample rate in samples/second
    #[clap(short, long, default_value_t = 4e6)]
    sample_rate: f64,
    /// Center frequency in Hz
    #[clap(short = 'f', long, default_value_t = 866e6)]
    freq: f64,
    /// Recording duration in seconds
    #[clap(short, long, default_value_t = 15.0)]
    duration: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Recording configuration: {args:?}");

    if let Some(parent) = Path::new(&args.output).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let n_samples = (args.duration * args.sample_rate).round() as u64;
    println!(
        "Capturing {n_samples} samples at {} Msps to {}",
        args.sample_rate / 1e6,
        args.output
    );

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    let src = Builder::new(args.args)?
        .frequency(args.freq)
        .sample_rate(args.sample_rate)
        .gain(args.gain)
        .antenna(args.antenna)
        .build_source()?;

    connect!(fg, src);
    let src_id: BlockId = src.into();
    let head = fg.add_block(Head::<Complex32>::new(n_samples));
    let sink = fg.add_block(FileSink::<Complex32>::new(args.output));

    fg.connect_dyn(src_id, "outputs[0]", &head, "input")?;
    fg.connect_dyn(head, "output", &sink, "input")?;

    rt.run(fg)?;
    println!("Capture complete.");

    Ok(())
}