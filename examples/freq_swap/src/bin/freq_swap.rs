use anyhow::Result;
use futuresdr::async_io::Timer;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::signal_source::SignalSourceBuilder;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

const DEVICE_ARGS: &str = "";
const RX_CHANNEL: usize = 0;
const TX_CHANNEL: usize = 1;
const SAMPLE_RATE_HZ: f64 = 4e6;
const RX_START_FREQ_HZ: f64 = 830e6;
const TX_START_FREQ_HZ: f64 = 828.5e6;
const TX_TARGET_FREQ_HZ: f64 = 831.5e6;
const RX_TARGET_FREQ_HZ: f64 = 832e6;
const RX_GAIN_DB: f64 = 0.0;
const TX_GAIN_DB: f64 = 40.0;
const TONE_OFFSET_HZ: f32 = 0.0;
const TONE_AMPLITUDE: f32 = 0.6;
const PRE_RETUNE_SECS: f64 = 0.2;
const TX_ONLY_OBSERVE_SECS: f64 = 0.2;
const POST_RX_RETUNE_SECS: f64 = 0.2;

const IQ_PATH: &str = "freq_swap.cf32";
const META_PATH: &str = "freq_swap.meta.json";

fn main() -> Result<()> {
    futuresdr::runtime::init();

    let mut fg = Flowgraph::new();

    let tone = fg.add_block(SignalSourceBuilder::<Complex32>::sin(
        TONE_OFFSET_HZ,
        SAMPLE_RATE_HZ as f32,
        TONE_AMPLITUDE,
        0.0,
    ));

    let tx = fg.add_block(
        Builder::new(DEVICE_ARGS)?
            .channel(TX_CHANNEL)
            .frequency(TX_START_FREQ_HZ)
            .sample_rate(SAMPLE_RATE_HZ)
            .gain(TX_GAIN_DB)
            .build_sink()?,
    );
    let tx_id: BlockId = (&tx).into();

    let rx = fg.add_block(
        Builder::new(DEVICE_ARGS)?
            .channel(RX_CHANNEL)
            .frequency(RX_START_FREQ_HZ)
            .sample_rate(SAMPLE_RATE_HZ)
            .gain(RX_GAIN_DB)
            .build_source()?,
    );
    let rx_id: BlockId = (&rx).into();

    let sink = fg.add_block(FileSink::<Complex32>::new(IQ_PATH));

    fg.connect_dyn(tone, "output", tx_id, "inputs[0]")?;
    fg.connect_dyn(rx_id, "outputs[0]", sink, "input")?;

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    println!("Loopback freq-swap example started — capturing RX IQ to {IQ_PATH}");
    println!(
        "TX ch{TX_CHANNEL}: {:.3} MSPS @ {:.3} MHz (start)",
        SAMPLE_RATE_HZ / 1e6,
        TX_START_FREQ_HZ / 1e6
    );
    println!(
        "RX ch{RX_CHANNEL}: {:.3} MSPS @ {:.3} MHz (start)",
        SAMPLE_RATE_HZ / 1e6,
        RX_START_FREQ_HZ / 1e6
    );
    println!(
        "Tone offset {:.0} Hz amp {:.2} → carrier @ {:.3} MHz",
        TONE_OFFSET_HZ,
        TONE_AMPLITUDE,
        (TX_START_FREQ_HZ + TONE_OFFSET_HZ as f64) / 1e6
    );
    println!(
        "Schedule: TX {:.3}→{:.3} MHz at t={:.2}s, RX {:.3}→{:.3} MHz at t={:.2}s, stop at t={:.2}s",
        TX_START_FREQ_HZ / 1e6,
        TX_TARGET_FREQ_HZ / 1e6,
        PRE_RETUNE_SECS,
        RX_START_FREQ_HZ / 1e6,
        RX_TARGET_FREQ_HZ / 1e6,
        PRE_RETUNE_SECS + TX_ONLY_OBSERVE_SECS,
        PRE_RETUNE_SECS + TX_ONLY_OBSERVE_SECS + POST_RX_RETUNE_SECS,
    );

    let total_secs = PRE_RETUNE_SECS + TX_ONLY_OBSERVE_SECS + POST_RX_RETUNE_SECS;

    let (tx_retune_offset, rx_retune_offset) = rt.block_on(async move {
        let t0 = Instant::now();

        Timer::after(Duration::from_secs_f64(PRE_RETUNE_SECS)).await;
        let tx_retune = t0.elapsed().as_secs_f64();
        handle
            .callback(tx_id, "freq", Pmt::F64(TX_TARGET_FREQ_HZ))
            .await
            .unwrap();
        println!("[t={tx_retune:.4}s] TX retuned to {:.3} MHz", TX_TARGET_FREQ_HZ / 1e6);

        Timer::after(Duration::from_secs_f64(TX_ONLY_OBSERVE_SECS)).await;
        let rx_retune = t0.elapsed().as_secs_f64();
        handle
            .callback(rx_id, "freq", Pmt::F64(RX_TARGET_FREQ_HZ))
            .await
            .unwrap();
        println!("[t={rx_retune:.4}s] RX retuned to {:.3} MHz", RX_TARGET_FREQ_HZ / 1e6);

        Timer::after(Duration::from_secs_f64(POST_RX_RETUNE_SECS)).await;
        println!("[t={:.4}s] stopping flowgraph", t0.elapsed().as_secs_f64());

        handle.terminate_and_wait().await.unwrap();

        (tx_retune, rx_retune)
    });

    let meta = format!(
        "{{\n  \"sample_rate_hz\": {SAMPLE_RATE_HZ},\n  \"rx_start_freq_hz\": {RX_START_FREQ_HZ},\n  \"tx_start_freq_hz\": {TX_START_FREQ_HZ},\n  \"tx_target_freq_hz\": {TX_TARGET_FREQ_HZ},\n  \"rx_target_freq_hz\": {RX_TARGET_FREQ_HZ},\n  \"tx_retune_t_s\": {tx_retune_offset},\n  \"rx_retune_t_s\": {rx_retune_offset},\n  \"capture_total_s\": {total_secs},\n  \"iq_path\": \"{IQ_PATH}\",\n  \"format\": \"cf32 (interleaved float32 I,Q little-endian)\"\n}}\n"
    );
    let mut f = File::create(META_PATH)?;
    f.write_all(meta.as_bytes())?;
    println!("Wrote metadata to {META_PATH}");

    Ok(())
}
