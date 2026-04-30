use clap::Parser;
use futuresdr::blocks::Apply;
use futuresdr::blocks::Combine;
use futuresdr::blocks::Delay;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FftDirection;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::FileSource;
use futuresdr::blocks::MessagePipe;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::blocks::WebsocketSinkBuilder;
use futuresdr::blocks::WebsocketSinkMode;
use futuresdr::prelude::*;

use wlan_ah::Decoder;
use wlan_ah::FrameEqualizer;
use wlan_ah::MovingAverage;
use wlan_ah::SyncLong;
use wlan_ah::SyncShort;
use wlan_ah::{FFT_SIZE, SYMBOL_LEN};

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// DC Offset
    #[clap(short, long, default_value_t = false)]
    dc_offset: bool,
    /// Input file path
    #[clap(short = 'i', long, default_value = "bin/2026-04-27-15-22-31_wlan_ah_905M_4Msps_10s.cf32")]
    file: String,
    /// Throttle rate in samples/sec (use high value like 1e9 for batch)
    #[clap(short, long, default_value_t = 1e9)]
    rate: f64,
    /// Output directory for the raw .cf32 / .f32 dumps consumed by plots/render.py.
    #[clap(long, default_value = "plots")]
    plot_dir: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    // ── Source ──────────────────────────────────────────────────────────
    // .cf32 file = native Complex<f32> (matches notebook's np.fromfile(..., 'complex64')).
    let source = FileSource::<Complex32>::new(&args.file, false);
    let throttle = futuresdr::blocks::Throttle::<Complex32>::new(args.rate);
    // Identity Apply kept so downstream `convert_id` still has a stable BlockId
    // and the optional DC-offset stage can chain in/out cleanly.
    let convert = Apply::<_, _, _>::new(|c: &Complex32| *c);
    connect!(fg, source > throttle > convert);
    let convert_id: BlockId = convert.into();

    // ── Optional DC offset removal ─────────────────────────────────────
    let (prev, output): (BlockId, &str) = if args.dc_offset {
        let mut avg_real = 0.0;
        let mut avg_img = 0.0;
        let ratio = 1.0e-5;
        let dc = fg.add_block(Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
            avg_real = ratio * (c.re - avg_real) + avg_real;
            avg_img = ratio * (c.im - avg_img) + avg_img;
            Complex32::new(c.re - avg_real, c.im - avg_img)
        }));
        let dc_id: BlockId = dc.into();
        fg.connect_dyn(convert_id, "output", dc_id, "input")?;
        (dc_id, "output")
    } else {
        (convert_id, "output")
    };

    // ── Schmidl-Cox short preamble detector ────────────────────────────
    // Window lengths match the analysis notebook at fs = 4 MSps:
    //   correlation window = 2·Ts − Tu/4               (`np.convolve` length)
    //   power-norm window  = Ts/2                      (notebook uses mean)
    //   delay              = Tu/4
    // M[n] = |Σ_{288} x·conj(x_delayed)| / Σ_{80} |x|²
    const STF_TS: usize = SYMBOL_LEN;                       // Ts = Tu + Tu/4
    const STF_DELAY: usize = FFT_SIZE / 4;                  // Tu/4 = 32 @ 4 MSps
    const STF_CORR_WIN: usize = 2 * SYMBOL_LEN - STF_DELAY; // 288 @ 4 MSps
    const STF_POWER_WIN: usize = SYMBOL_LEN / 2;            // 80  @ 4 MSps
    let delay = fg.add_block(Delay::<Complex32>::new(STF_DELAY as isize));
    fg.connect_dyn(prev, output, &delay, "input")?;

    let complex_to_mag_2 = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
    let float_avg = MovingAverage::<f32>::new(STF_POWER_WIN);
    fg.connect_dyn(prev, output, &complex_to_mag_2, "input")?;
    connect!(fg, complex_to_mag_2 > float_avg);

    let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &Complex32| a * b.conj(),
    ));
    let complex_avg = MovingAverage::<Complex32>::new(STF_CORR_WIN);
    fg.connect_dyn(prev, output, &mult_conj, "in0")?;
    connect!(fg, mult_conj > complex_avg;
                 delay > in1.mult_conj);

    // ── Dump complex_avg to disk ────────────────────────────────────────
    // complex_avg[n] is the running 288-sample correlation sum — the rust
    // analogue of the notebook's `stf_corr_complex` (np.convolve output).
    // Written as native Complex<f32>; plots/render.py loads it.
    std::fs::create_dir_all(&args.plot_dir).ok();
    let stf_corr_path = format!("{}/stf_corr.cf32", args.plot_dir);
    let stf_metric_path = format!("{}/stf_metric.f32", args.plot_dir);
    let stf_corr_sink = FileSink::<Complex32>::new(&stf_corr_path);
    connect!(fg, complex_avg > stf_corr_sink);

    let divide_mag = fg.add_block(
        Combine::<_, _, _, _>::new(|a: &Complex32, b: &f32| a.norm() / b),
    );
    connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);
    let divide_mag_id: BlockId = divide_mag.into();

    // ── Dump the streaming detection metric M[n] = |Σcorr|/Σpower to disk ──
    // The python script in plots/render.py applies the notebook's block
    // argmax + local-max + 30 dB threshold to this stream.
    let stf_metric_sink = fg.add_block(FileSink::<f32>::new(&stf_metric_path));
    fg.connect_dyn(divide_mag_id, "output", &stf_metric_sink, "input")?;

    // ── STF detection-metric printer ────────────────────────────────────
    // Faithful port of the notebook's detection rule:
    //
    //   detect_window     = 6·Ts                                  (= 960)
    //   local_maxima_win  = ±2·Ts around the block argmax         (= ±320)
    //   threshold         = 30 dB on `max_value_norm` (mean-based)
    //
    // Notebook code:
    //   stf_corr_w = stf_corr.reshape(-1, 6*Ts)
    //   for j: a = argmax(stf_corr_w[j])
    //          local_max = stf_corr_w[j,a] >= max(stf_corr[b ± 2·Ts])
    //          detect = max_value_norm >= 10**(30/10)
    //
    // Scale offset: rust metric is sum-based (|Σcorr|/Σpower), notebook is
    // mean-based (|Σcorr|/mean(power)). They differ by N_power = 80 →
    // 10·log10(80) ≈ 19.03 dB. So notebook's 30 dB threshold maps to
    // 30 - 19.03 ≈ 10.97 dB rust ≈ 12.5 linear.
    const DETECT_WIN: usize = 6 * STF_TS;   // 960
    const LOCAL_MAX_R: usize = 2 * STF_TS;  // 320
    const NEED: usize = DETECT_WIN + 2 * LOCAL_MAX_R; // 1600

    let nb_threshold_db: f32 = 30.0;
    let rust_threshold: f32 =
        10f32.powf(nb_threshold_db / 10.0) / (STF_POWER_WIN as f32); // = 12.5

    let mut buf: Vec<f32> = Vec::with_capacity(NEED + 16);
    let mut block_global_start: usize = 0; // metric-stream index of buf[0]
    let mut peak_count: usize = 0;

    println!(
        "[STF detector] threshold = {:.2} (linear)  ≈ {:+.2} dB rust ≈ {:+.2} dB nb-equiv",
        rust_threshold,
        10.0 * rust_threshold.log10(),
        10.0 * rust_threshold.log10() + 19.03
    );

    let metric_print = fg.add_block(Apply::<_, _, _>::new(move |&m: &f32| -> f32 {
        buf.push(m);
        while buf.len() >= NEED {
            // argmax inside the *middle* block (offset LOCAL_MAX_R, length DETECT_WIN)
            let mut max_val = f32::NEG_INFINITY;
            let mut max_rel = 0usize;
            for i in 0..DETECT_WIN {
                let v = buf[LOCAL_MAX_R + i];
                if v > max_val {
                    max_val = v;
                    max_rel = i;
                }
            }
            // local-max check: peak must be >= max in [arg − 2·Ts, arg + 2·Ts)
            let arg_buf = LOCAL_MAX_R + max_rel;
            let mut local_max = f32::NEG_INFINITY;
            for i in (arg_buf - LOCAL_MAX_R)..(arg_buf + LOCAL_MAX_R) {
                if buf[i] > local_max {
                    local_max = buf[i];
                }
            }
            let is_local_max = max_val >= local_max;
            let above_thresh = max_val >= rust_threshold;

            if above_thresh && is_local_max {
                let metric_idx = block_global_start + arg_buf;
                // x-sample-index of the START of the correlation window:
                //   subtract (STF_CORR_WIN - 1) + STF_DELAY = 287 + 32 = 319
                let x_start = metric_idx.saturating_sub(STF_CORR_WIN - 1 + STF_DELAY);
                let db_rust = 10.0 * max_val.log10();
                println!(
                    "STF peak #{:<3} block={:<4} metric_idx={:>9}  x_start≈{:>9}  M={:.4}  ({:+.2} dB rust / {:+.2} dB nb)",
                    peak_count,
                    block_global_start / DETECT_WIN,
                    metric_idx,
                    x_start,
                    max_val,
                    db_rust,
                    db_rust + 19.03
                );
                peak_count += 1;
            }

            // advance one block
            buf.drain(0..DETECT_WIN);
            block_global_start += DETECT_WIN;
        }
        m
    }));
    let metric_print_id: BlockId = metric_print.into();
    fg.connect_dyn(divide_mag_id, "output", metric_print_id, "input")?;
    // metric_print's output is consumed by cor_ws below (replaces direct
    // divide_mag → cor_ws connection so the tap is in-line with a sink).

    // ── Sync & decode ──────────────────────────────────────────────────
    let sync_short: SyncShort = SyncShort::new();
    connect!(fg, delay > in_sig.sync_short;
                 complex_avg > in_abs.sync_short);
    fg.connect_dyn(divide_mag_id, "output", &sync_short, "in_cor")?;

    // Tap: capture sync_short output (CFO-corrected time-domain stream that
    // sync_long sees). Useful to check whether the LTF preamble is intact.
    // Inserted in-line on sync_short → sync_long.
    let sync_short_out_path = format!("{}/sync_short_out.cf32", args.plot_dir);
    let sync_short_dump = fg.add_block(FileSink::<Complex32>::new(&sync_short_out_path));
    let sync_long: SyncLong = SyncLong::new();
    connect!(fg, sync_short > sync_long;
                 sync_short > sync_short_dump);

    let fft: Fft = Fft::new(FFT_SIZE);
    let frame_equalizer: FrameEqualizer = FrameEqualizer::new();
    let decoder = Decoder::new();
    connect!(fg, sync_long > fft > frame_equalizer > decoder);

    // ── Frame output ───────────────────────────────────────────────────
    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    let udp1 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55555");
    let udp2 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55556");
    connect!(fg, decoder.rx_frames | message_pipe;
                 decoder.rx_frames | udp1;
                 decoder.rftap | udp2);

    // ── First-PPDU capture (for plots/render.py) ───────────────────────
    // Subscribes to the message ports the FrameEqualizer already emits:
    //   channel_est       → VecCF32 of length N_ACTIVE_SC (= 56)
    //   preamble_symbols  → VecCF32 of length 48  (SIG / SIG-A symbol 1)
    //   preamble_symbols2 → VecCF32 of length 48  (SIG / SIG-A symbol 2)
    //   symbols           → VecCF32 of length N_sym × 52 (data symbols)
    // We take only the first message on each port — that's the first PPDU.
    let (chest_tx, mut chest_rx) = mpsc::channel::<Pmt>(64);
    let (sig1_tx, mut sig1_rx) = mpsc::channel::<Pmt>(64);
    let (sig2_tx, mut sig2_rx) = mpsc::channel::<Pmt>(64);
    let (data_tx, mut data_rx) = mpsc::channel::<Pmt>(64);
    let (ltf1_tx, mut ltf1_rx) = mpsc::channel::<Pmt>(64);
    let (stf_tx, mut stf_rx) = mpsc::channel::<Pmt>(64);
    let (cm_tx, mut cm_rx) = mpsc::channel::<Pmt>(64);
    let (lt_tx, mut lt_rx) = mpsc::channel::<Pmt>(64);
    let chest_pipe = MessagePipe::new(chest_tx);
    let sig1_pipe = MessagePipe::new(sig1_tx);
    let sig2_pipe = MessagePipe::new(sig2_tx);
    let data_pipe = MessagePipe::new(data_tx);
    let ltf1_pipe = MessagePipe::new(ltf1_tx);
    let stf_pipe = MessagePipe::new(stf_tx);
    let cm_pipe = MessagePipe::new(cm_tx);
    let lt_pipe = MessagePipe::new(lt_tx);
    connect!(fg, frame_equalizer.channel_est       | chest_pipe;
                 frame_equalizer.preamble_symbols  | sig1_pipe;
                 frame_equalizer.preamble_symbols2 | sig2_pipe;
                 frame_equalizer.symbols           | data_pipe;
                 frame_equalizer.ltf1_eq           | ltf1_pipe;
                 sync_long.stf_td                  | stf_pipe;
                 sync_long.corr_mag                | cm_pipe;
                 sync_long.ltf_td                  | lt_pipe);

    // ── Visualization sinks ────────────────────────────────────────────
    // Spectrogram (waterfall) — ws://127.0.0.1:9013
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
    fg.connect_dyn(convert_id, "output", fft_spec_id, "input")?;
    fg.connect_dyn(fft_spec_id, "output", fft_to_power_id, "input")?;
    fg.connect_dyn(fft_to_power_id, "output", spectrogram_ws_id, "input")?;

    // Constellation symbols — ws://127.0.0.1:9012
    let symbol_sink = WebsocketPmtSink::new(9012);
    connect!(fg, frame_equalizer.symbols | r#in.symbol_sink);

    // Channel estimate — ws://127.0.0.1:9016
    let h_est_sink = WebsocketPmtSink::new(9016);
    connect!(fg, frame_equalizer.channel_est | r#in.h_est_sink);

    // Correlation metric (time sink) — ws://127.0.0.1:9014
    // FixedDropping(256) matches spectrogram chunk size so they scroll together.
    let cor_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9014)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    fg.connect_dyn(metric_print_id, "output", cor_ws, "input")?;

    // Raw source magnitude (time sink) — ws://127.0.0.1:9015
    let src_mag = fg.add_block(Apply::<_, _, _>::new(|c: &Complex32| c.norm()));
    let src_ws = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9015)
            .mode(WebsocketSinkMode::FixedDropping(256))
            .build(),
    );
    let src_mag_id: BlockId = src_mag.into();
    let src_ws_id: BlockId = src_ws.into();
    fg.connect_dyn(convert_id, "output", src_mag_id, "input")?;
    fg.connect_dyn(src_mag_id, "output", src_ws_id, "input")?;

    // ── Run ────────────────────────────────────────────────────────────
    let plot_dir = args.plot_dir.clone();
    let (_fg, _handle) = rt.start_sync(fg)?;
    rt.block_on(async move {
        // Drain rx_frame as before — keeps the loop alive until the file
        // ends and all flowgraph senders close.
        while let Some(x) = rx_frame.next().await {
            match x {
                Pmt::Blob(data) => println!("received frame ({} bytes)", data.len()),
                _ => break,
            }
        }

        // Helper: extract a VecCF32 from the first message on a port.
        async fn first_veccf32(rx: &mut mpsc::Receiver<Pmt>) -> Option<Vec<Complex32>> {
            match rx.next().await {
                Some(Pmt::VecCF32(v)) => Some(v),
                _ => None,
            }
        }
        async fn first_vecf32(rx: &mut mpsc::Receiver<Pmt>) -> Option<Vec<f32>> {
            match rx.next().await {
                Some(Pmt::VecF32(v)) => Some(v),
                _ => None,
            }
        }
        let first_chest = first_veccf32(&mut chest_rx).await;
        let first_sig1 = first_veccf32(&mut sig1_rx).await;
        let first_sig2 = first_veccf32(&mut sig2_rx).await;
        let first_data = first_veccf32(&mut data_rx).await;
        let first_ltf1 = first_veccf32(&mut ltf1_rx).await;
        let first_stf = first_veccf32(&mut stf_rx).await;
        let first_corr_mag = first_vecf32(&mut cm_rx).await;
        let first_ltf_td = first_veccf32(&mut lt_rx).await;

        let write_cf32 = |path: &str, v: &[Complex32]| -> std::io::Result<()> {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    v.as_ptr() as *const u8,
                    v.len() * std::mem::size_of::<Complex32>(),
                )
            };
            std::fs::write(path, bytes)
        };

        if let Some(v) = first_chest.as_ref() {
            let p = format!("{}/h_est.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} subcarriers)", p, v.len());
        } else {
            eprintln!("[dump] no channel_est captured (no PPDU decoded)");
        }
        if let Some(v) = first_sig1.as_ref() {
            let p = format!("{}/sig1.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} points)", p, v.len());
        }
        if let Some(v) = first_sig2.as_ref() {
            let p = format!("{}/sig2.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} points)", p, v.len());
        }
        if let Some(v) = first_data.as_ref() {
            let p = format!("{}/data_syms.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} points = {} symbols × 52)", p, v.len(), v.len() / 52);
        }
        if let Some(v) = first_ltf1.as_ref() {
            let p = format!("{}/ltf1_eq.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} points = 2 × 56)", p, v.len());
        }
        if let Some(v) = first_stf.as_ref() {
            let p = format!("{}/stf_td.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} samples)", p, v.len());
        }
        if let Some(v) = first_ltf_td.as_ref() {
            let p = format!("{}/ltf_td.cf32", plot_dir);
            let _ = write_cf32(&p, v);
            println!("[dump] {} ({} samples)", p, v.len());
        }
        if let Some(v) = first_corr_mag.as_ref() {
            let p = format!("{}/sync_long_corr_mag.f32", plot_dir);
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    v.as_ptr() as *const u8,
                    v.len() * std::mem::size_of::<f32>(),
                )
            };
            let _ = std::fs::write(&p, bytes);
            println!("[dump] {} ({} samples = SyncLong correlation magnitudes)", p, v.len());
        }
    });

    println!("[dump] {}", stf_corr_path);
    println!("[dump] {}", stf_metric_path);
    println!("[dump] render with: python {}/render.py", args.plot_dir);

    Ok(())
}
