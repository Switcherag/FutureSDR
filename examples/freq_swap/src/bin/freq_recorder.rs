use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::seify::Builder;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    about = "Standalone RX recorder: captures IQ to a cf32 file, optionally retuning the driver once mid-capture."
)]
struct Args {
    /// Seify device args (e.g. soapy=pluto, soapy=hackrf, ...)
    #[arg(long, default_value = "")]
    device: String,

    /// RX channel index
    #[arg(long, default_value_t = 0)]
    channel: usize,

    /// Sample rate in Hz
    #[arg(long, default_value_t = 4_000_000.0)]
    sample_rate: f64,

    /// Initial RX frequency in Hz
    #[arg(long, default_value_t = 830_000_000.0)]
    start_freq_hz: f64,

    /// Target RX frequency in Hz (retuned to at --switch-ms; same as start = no retune)
    #[arg(long, default_value_t = 832_000_000.0)]
    target_freq_hz: f64,

    /// Delay before the driver retune, in milliseconds (ignored when start == target)
    #[arg(long, default_value_t = 100.0)]
    switch_ms: f64,

    /// Total capture duration in milliseconds
    #[arg(long, default_value_t = 500.0)]
    total_ms: f64,

    /// RX gain in dB
    #[arg(long, default_value_t = 0.0)]
    gain_db: f64,

    /// IQ output path (cf32, interleaved float32 little-endian)
    #[arg(long, default_value = "freq_recorder.cf32")]
    iq_path: String,

    /// Metadata JSON path
    #[arg(long, default_value = "freq_recorder.meta.json")]
    meta_path: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    futuresdr::runtime::init();

    let mut fg = Flowgraph::new();

    let rx = fg.add_block(
        Builder::new(&args.device)?
            .channel(args.channel)
            .frequency(args.start_freq_hz)
            .sample_rate(args.sample_rate)
            .gain(args.gain_db)
            .build_source()?,
    );
    let rx_id: BlockId = (&rx).into();

    let sink = fg.add_block(FileSink::<Complex32>::new(&args.iq_path));

    fg.connect_dyn(rx_id, "outputs[0]", sink, "input")?;

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    let do_retune = (args.target_freq_hz - args.start_freq_hz).abs() > f64::EPSILON;

    println!("freq_recorder — capturing RX IQ to {}", args.iq_path);
    println!("  device:        {}", args.device);
    println!(
        "  RX ch{}:       {:.3} MS/s @ {:.3} MHz",
        args.channel,
        args.sample_rate / 1e6,
        args.start_freq_hz / 1e6,
    );
    if do_retune {
        println!(
            "  retune:        -> {:.3} MHz at t={:.3} ms",
            args.target_freq_hz / 1e6,
            args.switch_ms,
        );
    } else {
        println!("  retune:        (none — start == target)");
    }
    println!("  gain:          {:.1} dB", args.gain_db);
    println!("  total:         {:.3} ms", args.total_ms);

    let switch_at = Duration::from_secs_f64(args.switch_ms / 1e3);
    let total = Duration::from_secs_f64(args.total_ms / 1e3);

    let retune_offset = rt.block_on(async move {
        let t0 = Instant::now();
        let mut retune_t = None;

        if do_retune && switch_at < total {
            Timer::after(switch_at).await;
            let t = t0.elapsed().as_secs_f64();
            handle
                .callback(rx_id, "freq", Pmt::F64(args.target_freq_hz))
                .await
                .unwrap();
            println!(
                "[t={t:.4}s] RX retuned to {:.3} MHz",
                args.target_freq_hz / 1e6
            );
            retune_t = Some(t);
        }

        let remaining = total.saturating_sub(t0.elapsed());
        Timer::after(remaining).await;
        println!("[t={:.4}s] stopping flowgraph", t0.elapsed().as_secs_f64());

        handle.terminate_and_wait().await.unwrap();
        retune_t
    });

    let retune_field = match retune_offset {
        Some(t) => format!("{t}"),
        None => "null".to_string(),
    };
    let meta = format!(
        "{{\n  \"device\": \"{}\",\n  \"channel\": {},\n  \"sample_rate_hz\": {},\n  \"start_freq_hz\": {},\n  \"target_freq_hz\": {},\n  \"switch_ms\": {},\n  \"retune_t_s\": {},\n  \"gain_db\": {},\n  \"capture_total_ms\": {},\n  \"iq_path\": \"{}\",\n  \"format\": \"cf32 (interleaved float32 I,Q little-endian)\"\n}}\n",
        args.device,
        args.channel,
        args.sample_rate,
        args.start_freq_hz,
        args.target_freq_hz,
        args.switch_ms,
        retune_field,
        args.gain_db,
        args.total_ms,
        args.iq_path,
    );
    let mut f = File::create(&args.meta_path)?;
    f.write_all(meta.as_bytes())?;
    println!("Wrote metadata to {}", args.meta_path);

    Ok(())
}
