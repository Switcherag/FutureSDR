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
    about = "Standalone RX recorder: captures IQ to a cf32 file while sweeping the RX sample rate (IQ bandwidth) through a list of values."
)]
struct Args {
    /// Seify device args (e.g. driver=bladerf, soapy=hackrf, ...)
    #[arg(long, default_value = "")]
    device: String,

    /// RX channel index
    #[arg(long, default_value_t = 0)]
    channel: usize,

    /// RX center frequency in Hz
    #[arg(long, default_value_t = 831_000_000.0)]
    freq_hz: f64,

    /// Comma-separated list of sample rates to sweep through, in MHz
    #[arg(long, default_value = "1,2,4,8,16")]
    bw_mhz: String,

    /// Dwell time on each sample rate before switching, in milliseconds
    #[arg(long, default_value_t = 200.0)]
    dwell_ms: f64,

    /// Settling time before the very first switch (also acts as initial capture window), in milliseconds
    #[arg(long, default_value_t = 200.0)]
    initial_ms: f64,

    /// RX gain in dB
    #[arg(long, default_value_t = 0.0)]
    gain_db: f64,

    /// IQ output path (cf32, interleaved float32 little-endian)
    #[arg(long, default_value = "bw_recorder.cf32")]
    iq_path: String,

    /// Metadata JSON path
    #[arg(long, default_value = "bw_recorder.meta.json")]
    meta_path: String,
}

fn parse_bw_list(s: &str) -> Result<Vec<f64>> {
    let mut out = Vec::new();
    for tok in s.split(',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        let v: f64 = t
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid bw entry '{t}': {e}"))?;
        if !(v > 0.0) {
            anyhow::bail!("bw entry must be > 0 MHz, got {v}");
        }
        out.push(v * 1e6);
    }
    if out.is_empty() {
        anyhow::bail!("--bw-mhz produced an empty list");
    }
    Ok(out)
}

fn main() -> Result<()> {
    let args = Args::parse();
    futuresdr::runtime::init();

    let bw_hz_list = parse_bw_list(&args.bw_mhz)?;
    let start_sr_hz = bw_hz_list[0];

    let mut fg = Flowgraph::new();

    let rx = fg.add_block(
        Builder::new(&args.device)?
            .channel(args.channel)
            .frequency(args.freq_hz)
            .sample_rate(start_sr_hz)
            .gain(args.gain_db)
            .build_source()?,
    );
    let rx_id: BlockId = (&rx).into();

    let sink = fg.add_block(FileSink::<Complex32>::new(&args.iq_path));

    fg.connect_dyn(rx_id, "outputs[0]", sink, "input")?;

    let rt = Runtime::new();
    let (_task, mut handle) = rt.start_sync(fg)?;

    println!("bw_recorder — sweeping RX sample rate (IQ bandwidth)");
    println!("  device:        {}", args.device);
    println!("  RX ch{}:       LO {:.3} MHz", args.channel, args.freq_hz / 1e6);
    println!(
        "  start rate:    {:.3} MS/s",
        start_sr_hz / 1e6,
    );
    println!(
        "  sweep ({} steps): {}",
        bw_hz_list.len(),
        bw_hz_list
            .iter()
            .map(|v| format!("{:.3}", v / 1e6))
            .collect::<Vec<_>>()
            .join(" -> "),
    );
    println!("  initial dwell: {:.3} ms", args.initial_ms);
    println!("  step dwell:    {:.3} ms", args.dwell_ms);
    println!("  gain:          {:.1} dB", args.gain_db);

    let initial = Duration::from_secs_f64(args.initial_ms / 1e3);
    let dwell = Duration::from_secs_f64(args.dwell_ms / 1e3);

    let sweep = bw_hz_list.clone();
    let switch_events = rt.block_on(async move {
        let t0 = Instant::now();
        let mut events: Vec<(f64, f64, Option<String>)> = Vec::new();

        events.push((0.0, start_sr_hz, None));

        Timer::after(initial).await;

        for sr_hz in sweep.iter().skip(1) {
            let t_req = t0.elapsed().as_secs_f64();
            let res = handle
                .callback(rx_id, "sample_rate", Pmt::F64(*sr_hz))
                .await;
            let t_done = t0.elapsed().as_secs_f64();
            match res {
                Ok(_) => {
                    println!(
                        "[t={t_done:.4}s] RX sample rate -> {:.3} MS/s  (req at t={t_req:.4}s, dt={:.3} ms)",
                        sr_hz / 1e6,
                        (t_done - t_req) * 1e3,
                    );
                    events.push((t_done, *sr_hz, None));
                }
                Err(e) => {
                    let msg = format!("{e}");
                    println!(
                        "[t={t_done:.4}s] RX sample rate -> {:.3} MS/s FAILED: {msg}",
                        sr_hz / 1e6,
                    );
                    events.push((t_done, *sr_hz, Some(msg)));
                }
            }
            Timer::after(dwell).await;
        }

        let t_stop = t0.elapsed().as_secs_f64();
        println!("[t={t_stop:.4}s] stopping flowgraph");
        handle.terminate_and_wait().await.unwrap();
        (events, t_stop)
    });

    let (events, total_s) = switch_events;

    let mut events_json = String::new();
    for (i, (t, sr, err)) in events.iter().enumerate() {
        if i > 0 {
            events_json.push_str(",\n");
        }
        let err_field = match err {
            Some(m) => format!("\"{}\"", m.replace('\\', "\\\\").replace('"', "\\\"")),
            None => "null".to_string(),
        };
        events_json.push_str(&format!(
            "    {{\"t_s\": {t}, \"sample_rate_hz\": {sr}, \"error\": {err_field}}}",
        ));
    }

    let meta = format!(
        "{{\n  \"device\": \"{}\",\n  \"channel\": {},\n  \"freq_hz\": {},\n  \"gain_db\": {},\n  \"initial_ms\": {},\n  \"dwell_ms\": {},\n  \"capture_total_s\": {},\n  \"iq_path\": \"{}\",\n  \"format\": \"cf32 (interleaved float32 I,Q little-endian)\",\n  \"sample_rate_sweep_hz\": [{}],\n  \"switch_events\": [\n{}\n  ]\n}}\n",
        args.device,
        args.channel,
        args.freq_hz,
        args.gain_db,
        args.initial_ms,
        args.dwell_ms,
        total_s,
        args.iq_path,
        bw_hz_list
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        events_json,
    );
    let mut f = File::create(&args.meta_path)?;
    f.write_all(meta.as_bytes())?;
    println!("Wrote metadata to {}", args.meta_path);

    Ok(())
}
