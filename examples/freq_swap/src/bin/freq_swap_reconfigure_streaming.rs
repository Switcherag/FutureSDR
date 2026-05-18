use anyhow::Result;
use clap::Parser;
use freq_swap::BasebandOscillator;
use futuresdr::async_io::Timer;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::seify::Builder;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

const DEVICE_ARGS: &str = "";
const RX_CHANNEL: usize = 0;
const TX_CHANNEL: usize = 1;
const SAMPLE_RATE_HZ: f64 = 4e6;

const TX_FREQ_A_HZ: f64 = 830.5e6;
const TX_FREQ_B_HZ: f64 = 831.5e6;
const TX_LO_HZ: f64 = (TX_FREQ_A_HZ + TX_FREQ_B_HZ) / 2.0;
const TX_BB_A_HZ: f32 = (TX_FREQ_A_HZ - TX_LO_HZ) as f32;
const TX_BB_B_HZ: f32 = (TX_FREQ_B_HZ - TX_LO_HZ) as f32;

const RX_START_FREQ_HZ: f64 = 830.0e6;
const RX_TARGET_FREQ_HZ: f64 = 832.0e6;

const RX_GAIN_DB: f64 = 0.0;
const TX_GAIN_DB: f64 = 40.0;
const TONE_AMPLITUDE: f32 = 0.6;

const IQ_PATH: &str = "freq_swap_reconfigure_streaming.cf32";
const META_PATH: &str = "freq_swap_reconfigure_streaming.meta.json";

#[derive(Parser, Debug)]
#[command(
    about = "TX LO is fixed at the midpoint; baseband tone hops every --tx-period-ms so the emitted carrier toggles between TX_FREQ_A and TX_FREQ_B. RX driver retunes once after --rx-switch-ms."
)]
struct Args {
    /// Baseband-hop period in milliseconds.
    #[arg(long, default_value_t = 10.0)]
    tx_period_ms: f64,

    /// Delay before the RX driver retune, in milliseconds.
    #[arg(long, default_value_t = 100.0)]
    rx_switch_ms: f64,

    /// Total capture duration in milliseconds.
    #[arg(long, default_value_t = 500.0)]
    total_ms: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    futuresdr::runtime::init();

    let mut fg = Flowgraph::new();

    let osc = fg.add_block(BasebandOscillator::<DefaultCpuWriter<Complex32>>::new(
        TX_BB_A_HZ,
        SAMPLE_RATE_HZ as f32,
        TONE_AMPLITUDE,
    ));
    let osc_id: BlockId = (&osc).into();

    let tx = fg.add_block(
        Builder::new(DEVICE_ARGS)?
            .channel(TX_CHANNEL)
            .frequency(TX_LO_HZ)
            .sample_rate(SAMPLE_RATE_HZ)
            .gain(TX_GAIN_DB)
            .build_sink()?,
    );

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

    fg.connect_dyn(osc, "output", tx, "inputs[0]")?;
    fg.connect_dyn(rx_id, "outputs[0]", sink, "input")?;

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    println!(
        "freq_swap_reconfigure_streaming — capturing RX IQ to {IQ_PATH}"
    );
    println!(
        "TX ch{TX_CHANNEL}: {:.3} MSPS, LO fixed @ {:.3} MHz, baseband {:+.3}<->{:+.3} MHz every {:.3} ms (RF: {:.3}<->{:.3} MHz)",
        SAMPLE_RATE_HZ / 1e6,
        TX_LO_HZ / 1e6,
        TX_BB_A_HZ / 1e6,
        TX_BB_B_HZ / 1e6,
        args.tx_period_ms,
        TX_FREQ_A_HZ / 1e6,
        TX_FREQ_B_HZ / 1e6,
    );
    println!(
        "RX ch{RX_CHANNEL}: {:.3} MSPS @ {:.3} MHz, driver retune to {:.3} MHz at t={:.3} ms",
        SAMPLE_RATE_HZ / 1e6,
        RX_START_FREQ_HZ / 1e6,
        RX_TARGET_FREQ_HZ / 1e6,
        args.rx_switch_ms,
    );
    println!("Total capture: {:.3} ms", args.total_ms);

    let tx_period = Duration::from_secs_f64(args.tx_period_ms / 1e3);
    let rx_switch_at = Duration::from_secs_f64(args.rx_switch_ms / 1e3);
    let total = Duration::from_secs_f64(args.total_ms / 1e3);

    let t0 = Instant::now();

    let mut osc_handle = handle.clone();
    let osc_task = rt.spawn(async move {
        let mut on_b = false;
        let mut hops: Vec<(f64, f32, f64)> = Vec::new();
        let start = Instant::now();
        loop {
            Timer::after(tx_period).await;
            if start.elapsed() >= total {
                break;
            }
            on_b = !on_b;
            let bb_freq = if on_b { TX_BB_B_HZ } else { TX_BB_A_HZ };
            if osc_handle
                .callback(osc_id, "freq", Pmt::F32(bb_freq))
                .await
                .is_err()
            {
                break;
            }
            let rf = TX_LO_HZ + bb_freq as f64;
            hops.push((start.elapsed().as_secs_f64(), bb_freq, rf));
        }
        hops
    });

    let (tx_hops, rx_retune_offset) = rt.block_on(async move {
        Timer::after(rx_switch_at).await;
        let rx_retune = t0.elapsed().as_secs_f64();
        handle
            .callback(rx_id, "freq", Pmt::F64(RX_TARGET_FREQ_HZ))
            .await
            .unwrap();
        println!(
            "[t={rx_retune:.4}s] RX retuned to {:.3} MHz",
            RX_TARGET_FREQ_HZ / 1e6
        );

        let remaining = total.saturating_sub(t0.elapsed());
        Timer::after(remaining).await;
        println!("[t={:.4}s] stopping flowgraph", t0.elapsed().as_secs_f64());

        let hops = osc_task.await;
        handle.terminate_and_wait().await.unwrap();

        (hops, rx_retune)
    });

    let hops_json: Vec<String> = tx_hops
        .iter()
        .map(|(t, bb, rf)| {
            format!("    {{\"t_s\": {t}, \"baseband_hz\": {bb}, \"rf_hz\": {rf}}}")
        })
        .collect();
    let hops_block = hops_json.join(",\n");

    let meta = format!(
        "{{\n  \"sample_rate_hz\": {SAMPLE_RATE_HZ},\n  \"tx_lo_hz\": {TX_LO_HZ},\n  \"tx_baseband_a_hz\": {TX_BB_A_HZ},\n  \"tx_baseband_b_hz\": {TX_BB_B_HZ},\n  \"tx_rf_a_hz\": {TX_FREQ_A_HZ},\n  \"tx_rf_b_hz\": {TX_FREQ_B_HZ},\n  \"tx_period_ms\": {},\n  \"rx_start_freq_hz\": {RX_START_FREQ_HZ},\n  \"rx_target_freq_hz\": {RX_TARGET_FREQ_HZ},\n  \"rx_switch_ms\": {},\n  \"rx_retune_t_s\": {rx_retune_offset},\n  \"capture_total_ms\": {},\n  \"iq_path\": \"{IQ_PATH}\",\n  \"format\": \"cf32 (interleaved float32 I,Q little-endian)\",\n  \"tx_hops\": [\n{hops_block}\n  ]\n}}\n",
        args.tx_period_ms, args.rx_switch_ms, args.total_ms
    );
    let mut f = File::create(META_PATH)?;
    f.write_all(meta.as_bytes())?;
    println!("Wrote metadata to {META_PATH}");

    Ok(())
}
