// retune_timing — measure SDR retune latency end-to-end, with an in-IQ marker.
//
// Builds: SeifySource -> Marker -> FileSink. At t = --switch-ms the binary
// fires (back-to-back, on the same Rust line) a `mark` message to the
// Marker block AND a `freq` message to the SeifySource block. The recorded
// IQ then contains:
//
//   - a high-amplitude pulse at the sample where the `mark` message was
//     serviced (proxy for "the moment the message reached the IQ chain")
//   - a frequency-shift transient at the sample where the hardware actually
//     retuned (the tone disappears or shifts)
//
// In post-processing you measure the gap between (1) and (2):
//
//   gap ≈ (T_seify_msg_delivery + hardware_retune) - T_marker_msg_delivery
//
// Both messages travel the same mpsc inbox so the two msg-delivery times
// are close — what's left is dominated by the hardware retune itself.
// That number is the floor you cannot go below without a faster radio.
//
// Tip: use a small TX-tone offset (e.g. retune by 100 kHz, not MHz) so the
// tone visibly shifts in band instead of vanishing — easier to see the
// transient and where it settles.
//
// Output: <iq_path> (cf32) + <meta_path> (json with timestamps & params).

use anyhow::Result;
use clap::Parser;
use freq_swap::Marker;
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
    about = "RX recorder with an in-IQ marker pulse fired alongside the freq message. \
             Reveals SDR retune latency vs flowgraph message-delivery latency in a single capture."
)]
struct Args {
    /// Seify device args (e.g. "", "soapy=hackrf", "driver=rtlsdr")
    #[arg(long, default_value = "")]
    device: String,

    /// RX channel index
    #[arg(long, default_value_t = 0)]
    channel: usize,

    /// Sample rate in Hz
    #[arg(long, default_value_t = 8_000_000.0)]
    sample_rate: f64,

    /// Initial RX frequency in Hz
    #[arg(long, default_value_t = 830_000_000.0)]
    start_freq_hz: f64,

    /// Target RX frequency in Hz (retuned to at --switch-ms)
    #[arg(long, default_value_t = 832_100_000.0)]
    target_freq_hz: f64,

    /// Delay before the retune fires, in milliseconds
    #[arg(long, default_value_t = 100.0)]
    switch_ms: f64,

    /// Total capture duration in milliseconds
    #[arg(long, default_value_t = 1000.0)]
    total_ms: f64,

    /// RX gain in dB
    #[arg(long, default_value_t = 0.0)]
    gain_db: f64,

    /// Marker amplitude (I component). The marker pulse is `(mark_amp, 0)`
    /// for `mark_samples` consecutive samples. Should be well above any
    /// signal you expect to see (default 10.0 is far outside [-1, 1]).
    #[arg(long, default_value_t = 10.0)]
    mark_amp: f32,

    /// Marker pulse length, in samples. Keep small (a handful of samples)
    /// so the marker doesn't overlap the retune transient.
    #[arg(long, default_value_t = 8)]
    mark_samples: usize,

    /// IQ output path (cf32, interleaved float32 little-endian)
    #[arg(long, default_value = "retune_timing.cf32")]
    iq_path: String,

    /// Metadata JSON path
    #[arg(long, default_value = "retune_timing.meta.json")]
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

    let marker = fg.add_block(Marker::<
        futuresdr::prelude::DefaultCpuReader<Complex32>,
        futuresdr::prelude::DefaultCpuWriter<Complex32>,
    >::new(
        Complex32::new(args.mark_amp, 0.0),
        args.mark_samples,
    ));
    let marker_id: BlockId = (&marker).into();

    let sink = fg.add_block(FileSink::<Complex32>::new(&args.iq_path));

    fg.connect_dyn(rx_id, "outputs[0]", marker_id, "input")?;
    fg.connect_dyn(marker_id, "output", sink, "input")?;

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    println!("retune_timing — capturing RX IQ with in-band marker to {}", args.iq_path);
    println!("  device:        {}", args.device);
    println!(
        "  RX ch{}:       {:.3} MS/s @ {:.6} MHz",
        args.channel,
        args.sample_rate / 1e6,
        args.start_freq_hz / 1e6,
    );
    println!(
        "  retune:        -> {:.6} MHz at t={:.3} ms (Δ = {:+.3} kHz)",
        args.target_freq_hz / 1e6,
        args.switch_ms,
        (args.target_freq_hz - args.start_freq_hz) / 1e3,
    );
    println!(
        "  marker:        ({:.3}, 0) × {} samples ≈ {:.2} µs of marker",
        args.mark_amp,
        args.mark_samples,
        args.mark_samples as f64 / args.sample_rate * 1e6,
    );
    println!("  total:         {:.3} ms", args.total_ms);

    let switch_at = Duration::from_secs_f64(args.switch_ms / 1e3);
    let total = Duration::from_secs_f64(args.total_ms / 1e3);

    let timings = rt.block_on(async move {
        let t0 = Instant::now();

        // Wait until the switch moment.
        Timer::after(switch_at).await;

        // Fire MARK and FREQ as close together as possible (back-to-back
        // awaits — they both push to the same flowgraph inbox).
        let t_mark_fire = t0.elapsed().as_secs_f64();
        handle.callback(marker_id, "mark", Pmt::Ok).await.unwrap();
        let t_mark_acked = t0.elapsed().as_secs_f64();

        let t_freq_fire = t0.elapsed().as_secs_f64();
        handle
            .callback(rx_id, "freq", Pmt::F64(args.target_freq_hz))
            .await
            .unwrap();
        let t_freq_acked = t0.elapsed().as_secs_f64();

        println!(
            "[t={t_mark_fire:.6}s] mark fired   (ack at {t_mark_acked:.6}s, Δ = {:.3} ms)",
            (t_mark_acked - t_mark_fire) * 1e3,
        );
        println!(
            "[t={t_freq_fire:.6}s] freq fired   (ack at {t_freq_acked:.6}s, Δ = {:.3} ms)",
            (t_freq_acked - t_freq_fire) * 1e3,
        );

        // Capture the rest.
        let remaining = total.saturating_sub(t0.elapsed());
        Timer::after(remaining).await;
        let t_stop = t0.elapsed().as_secs_f64();
        println!("[t={t_stop:.6}s] stopping flowgraph");
        handle.terminate_and_wait().await.unwrap();

        (t_mark_fire, t_mark_acked, t_freq_fire, t_freq_acked, t_stop)
    });

    let (t_mark_fire, t_mark_acked, t_freq_fire, t_freq_acked, t_stop) = timings;

    // Hand-rolled JSON so we don't need a serde dep for this one example.
    let meta = format!(
        "{{\n  \
            \"device\": \"{device}\",\n  \
            \"channel\": {channel},\n  \
            \"sample_rate_hz\": {sample_rate},\n  \
            \"start_freq_hz\": {start_freq},\n  \
            \"target_freq_hz\": {target_freq},\n  \
            \"switch_ms\": {switch_ms},\n  \
            \"gain_db\": {gain},\n  \
            \"mark_amp\": {mark_amp},\n  \
            \"mark_samples\": {mark_samples},\n  \
            \"capture_total_ms\": {total_ms},\n  \
            \"t_mark_fire_s\": {t_mark_fire},\n  \
            \"t_mark_acked_s\": {t_mark_acked},\n  \
            \"t_freq_fire_s\": {t_freq_fire},\n  \
            \"t_freq_acked_s\": {t_freq_acked},\n  \
            \"t_stop_s\": {t_stop},\n  \
            \"iq_path\": \"{iq_path}\",\n  \
            \"format\": \"cf32 (interleaved float32 I,Q little-endian)\"\n\
        }}\n",
        device = args.device,
        channel = args.channel,
        sample_rate = args.sample_rate,
        start_freq = args.start_freq_hz,
        target_freq = args.target_freq_hz,
        switch_ms = args.switch_ms,
        gain = args.gain_db,
        mark_amp = args.mark_amp,
        mark_samples = args.mark_samples,
        total_ms = args.total_ms,
        iq_path = args.iq_path,
    );
    let mut f = File::create(&args.meta_path)?;
    f.write_all(meta.as_bytes())?;
    println!("Wrote metadata to {}", args.meta_path);

    Ok(())
}
