use anyhow::Result;
use clap::Parser;
use freq_swap::SineFmOscillator;
use futuresdr::async_io::Timer;
use futuresdr::blocks::seify::Builder;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    about = "Standalone TX: emits a complex baseband tone whose instantaneous frequency itself oscillates sinusoidally — RF carrier is FM-modulated by a sine."
)]
struct Args {
    /// Seify device args (e.g. soapy=pluto, soapy=hackrf, driver=plutosdr,...)
    #[arg(long, default_value = "soapy=pluto")]
    device: String,

    /// TX channel index
    #[arg(long, default_value_t = 0)]
    channel: usize,

    /// Sample rate in Hz
    #[arg(long, default_value_t = 1_000_000.0)]
    sample_rate: f64,

    /// Hardware LO (RF center) in Hz
    #[arg(long, default_value_t = 831_000_000.0)]
    lo_hz: f64,

    /// Baseband mean frequency in Hz (offset from LO around which the FM sweeps)
    #[arg(long, default_value_t = 0.0)]
    center_hz: f32,

    /// Peak FM deviation in Hz (instantaneous freq = center ± deviation).
    /// Must satisfy |center| + |deviation| < sample_rate / 2 or the tone
    /// will alias around Nyquist (visible as the swept tone wrapping back
    /// into the visible band).
    #[arg(long, default_value_t = 200_000.0)]
    deviation_hz: f32,

    /// FM sweep period in milliseconds (one full sine of the inst. frequency)
    #[arg(long, default_value_t = 10.0)]
    period_ms: f64,

    /// Output amplitude (0..1)
    #[arg(long, default_value_t = 0.6)]
    amplitude: f32,

    /// TX gain in dB
    #[arg(long, default_value_t = 40.0)]
    gain_db: f64,

    /// Total run duration in seconds (0 = run until Ctrl-C)
    #[arg(long, default_value_t = 0.0)]
    duration_s: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    futuresdr::runtime::init();

    // Anti-alias guard: the instantaneous baseband frequency must fit
    // inside Nyquist (sample_rate/2) with a small margin, otherwise the
    // swept tone wraps around and you see overlapping aliased copies in
    // the spectrum.
    let nyquist = (args.sample_rate / 2.0) as f32;
    let bb_max = args.center_hz.abs() + args.deviation_hz.abs();
    let margin = 0.95 * nyquist;
    if bb_max > margin {
        anyhow::bail!(
            "baseband swing ({:.3} kHz = |center| + |deviation|) exceeds 95% of Nyquist \
             ({:.3} kHz at fs = {:.3} MS/s) — the tone will alias. \
             Either raise --sample-rate, lower --deviation-hz, or move --center-hz closer to 0.",
            bb_max / 1e3,
            margin / 1e3,
            args.sample_rate / 1e6,
        );
    }

    let mut fg = Flowgraph::new();

    let osc = fg.add_block(SineFmOscillator::<DefaultCpuWriter<Complex32>>::new(
        args.center_hz,
        args.deviation_hz,
        (args.period_ms / 1e3) as f32,
        args.sample_rate as f32,
        args.amplitude,
    ));

    let tx = fg.add_block(
        Builder::new(&args.device)?
            .channel(args.channel)
            .frequency(args.lo_hz)
            .sample_rate(args.sample_rate)
            .gain(args.gain_db)
            .build_sink()?,
    );

    fg.connect_dyn(osc, "output", tx, "inputs[0]")?;

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    println!("freq_sine_tx — sinusoidally FM-modulated tone");
    println!("  device:        {}", args.device);
    println!("  sample rate:   {:.3} MS/s", args.sample_rate / 1e6);
    println!("  LO:            {:.3} MHz", args.lo_hz / 1e6);
    println!(
        "  baseband:      center {:+.3} kHz ± {:.3} kHz, period {:.3} ms",
        args.center_hz / 1e3,
        args.deviation_hz / 1e3,
        args.period_ms,
    );
    println!(
        "  baseband swing: [{:+.3}, {:+.3}] kHz (Nyquist ±{:.3} kHz, margin used {:.1}%)",
        (args.center_hz - args.deviation_hz) / 1e3,
        (args.center_hz + args.deviation_hz) / 1e3,
        nyquist / 1e3,
        100.0 * bb_max / nyquist,
    );
    println!(
        "  RF sweep:      {:.3} MHz ↔ {:.3} MHz (mean {:.3} MHz)",
        (args.lo_hz + args.center_hz as f64 - args.deviation_hz as f64) / 1e6,
        (args.lo_hz + args.center_hz as f64 + args.deviation_hz as f64) / 1e6,
        (args.lo_hz + args.center_hz as f64) / 1e6,
    );
    println!("  gain:          {:.1} dB", args.gain_db);
    if args.duration_s > 0.0 {
        println!("  duration:      {:.3} s", args.duration_s);
    } else {
        println!("  duration:      until Ctrl-C");
    }

    rt.block_on(async move {
        if args.duration_s > 0.0 {
            Timer::after(Duration::from_secs_f64(args.duration_s)).await;
            handle.terminate_and_wait().await.unwrap();
        } else {
            futuresdr::futures::future::pending::<()>().await;
        }
    });

    Ok(())
}
