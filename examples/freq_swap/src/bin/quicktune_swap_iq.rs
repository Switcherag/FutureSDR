// quicktune_swap_iq — how long the link is actually off the air across a quick tune.
//
// `quick_tune_probe` and `retune_matrix_brf` time how long the *call* takes to
// return. That is a different question from how long the radio is unusable: the
// call can return before the RFIC has settled, or long after it has. This
// binary answers the on-air version, and it measures it in the received IQ.
//
// The experiment runs on one bladeRF in full duplex. With the defaults:
//
//   RX1 parks on 919 MHz and records continuously.
//   TX2 parks on 920 MHz and transmits a continuous carrier, so what the
//   receiver hears sits at +1 MHz in its baseband. The megahertz matters: on
//   the receiver's own LO the carrier would land on top of its DC offset and LO
//   leakage, and those do not go away during a retune — the outage being
//   measured would partly fill in with the receiver's own spurs.
//
//   The pair keeps that offset across every swap: 919/920, then 2425/2426.
//
//   At a known transmit timestamp the tone amplitude jumps to
//   `--spike-amplitude` for `--spike-samples`. That spike is the fiducial: it
//   leaves the DAC at an exact sample timestamp and it lands in the recording,
//   which is what ties the transmit clock to the recording's time axis.
//
//   `--gap-samples` after the spike starts, a quick-tune recall fires on *both*
//   channels, moving the pair up to 2425/2426 MHz. Both retunes are scheduled
//   at a sample timestamp rather than issued from the host, so neither inherits
//   USB jitter. `--dwell-ms` later the pair swaps back and the stream stops.
//
//   So the default run is: settle at 919/920, marker, swap to 2425/2426, wait
//   10 ms, swap back, quit. `--swaps` continues the alternation past that if
//   repeats are wanted; each swap gets its own marker.
//
// Reading the result: the spike sits at a known position in the recording, the
// retune fired exactly `--gap-samples` later, and the tone reappears once both
// LOs have landed on the new band. That last interval is the number — the time
// the link was down, from the sample the retune fired to the sample the tone
// came back.
//
// ── Tying the two clocks together ────────────────────────────────────────────
// RX and TX keep separate timestamp counters. Reading both over USB pins their
// offset no better than a USB round trip, which is far coarser than the thing
// being measured. So the first spike is a calibration spike with no retune
// behind it: transmitted at a known TX timestamp, found at a measured RX
// timestamp, and the difference is the offset. Every later event uses it, and
// that is what lets the RX retune be scheduled at the same physical instant as
// the TX one.
//
// The offset absorbs the receiver's group delay, so the RX retune fires that
// much late relative to the TX one — a few microseconds, and the only
// systematic in the alignment. The measured interval itself is immune: spike
// and recovery are read out of the same recording, so the group delay cancels.
//
// ── Why not `Marker` ─────────────────────────────────────────────────────────
// `src/marker.rs` is the usual way to put a pulse in the IQ here, and this is
// the same idea, but it cannot be that block. Two reasons, and the second is the
// important one:
//
//   `Marker` is a FutureSDR block and only exists inside a flowgraph. This
//   binary has no flowgraph: quick tune lives behind `bladerf_schedule_retune`
//   and timestamped streaming behind the metadata formats, and seify/SoapySDR
//   exposes neither, which is why the whole thing talks to libbladeRF directly.
//
//   `Marker` puts its pulse at the sample where the *message* was serviced —
//   its own doc comment says so, because exposing that delay is what it is for.
//   Here the spike is a clock reference, and a reference is only worth having if
//   the instant it marks is known exactly. So the spike is written into the
//   transmit burst at a chosen sample timestamp, and the retune is scheduled a
//   fixed number of samples behind it. Nothing is left to when a message
//   happened to be delivered.
//
// ── Two things to know before trusting a number ──────────────────────────────
// The receiver runs at 0 dB with manual gain control forced on, matching
// `retune_timing` and `freq_swap_reconfigure_streaming`; the transmitter runs at
// 40 dB, matching `freq_sine_tx`. Manual matters as much as the number: an AGC
// left running would chase the spike and then chase the outage, and a gain ramp
// settling after the swap is indistinguishable from an LO settling after the
// swap.
//
// `bladerf_get_quick_tune` needs the radio already tuned to the frequency being
// captured, so all four profiles (two bands x two directions) are taken up
// front, before the streams start moving.
//
// Run:
//   cd examples/freq_swap
//   BLADERF_INCLUDE_PATH=/usr/local/include RUSTFLAGS="-L/usr/local/lib64" \
//       cargo run --release --bin quicktune_swap_iq
//
// Output:
//   quicktune_swap.sc16       interleaved int16 IQ, gapless on the RX timestamp axis
//   quicktune_swap.meta.json  sample rate, event timestamps, measured latencies
//
// Set QTSWAP_DEBUG=1 to also print where any dropped samples landed. The summary
// counts them either way, but only the position says whether a swap was hit.
// Plot:
//   python3 plot_quicktune_swap_iq.py

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ::bladerf::sys::*;
use ::bladerf::{BladeRF, BladeRfAny, Channel, ComplexI16, GainMode, TuningMode};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Measure on-air quick-tune latency by recording the swap in IQ.")]
struct Args {
    /// Band A, where the run starts and ends. Hz.
    #[arg(long, default_value_t = 919_000_000)]
    freq_a: u64,
    /// Band B, the far side of the swap. Hz.
    #[arg(long, default_value_t = 2_425_000_000)]
    freq_b: u64,
    /// Hardware sample rate, Hz. This is the resolution of the answer: one
    /// sample is 1/rate seconds. Full duplex, so the USB carries twice this.
    /// 4 MSps to match the rest of the examples here.
    #[arg(long, default_value_t = 4e6)]
    sample_rate: f64,
    /// RF bandwidth, Hz. Defaults to the sample rate. Wider settles faster,
    /// which sharpens the recovery edge.
    #[arg(long)]
    bandwidth: Option<f64>,

    /// How far above the receiver the transmitter sits, Hz. RX parks on
    /// `--freq-a`, TX on `--freq-a + this`, and the pair keeps the offset across
    /// every swap — so with the defaults it is RX 919 / TX 920, then RX 2425 /
    /// TX 2426.
    ///
    /// This is what keeps the carrier off DC. Transmit on the receiver's own LO
    /// and the tone lands on top of the receiver's DC offset and LO leakage,
    /// which do not go away during a retune — the outage would partly fill in
    /// with the receiver's own spurs. An offset LO puts the carrier at a clean
    /// baseband frequency where nothing else lives.
    #[arg(long, default_value_t = 1e6)]
    tx_offset_hz: f64,
    /// Extra baseband offset on top of the LO offset, Hz. 0 transmits a plain
    /// carrier at the TX LO, which the LO offset alone already places clear of
    /// the receiver's DC.
    #[arg(long, default_value_t = 0.0)]
    tone_hz: f64,
    /// Tone amplitude as a fraction of full scale. Digital baseband scale, which
    /// is the only level control here — every RF gain stage is pinned at 0 dB.
    #[arg(long, default_value_t = 0.2)]
    amplitude: f64,
    /// Spike amplitude as a fraction of full scale. Must stand clear of the
    /// tone; the calibration search keys on it being the loudest thing present.
    #[arg(long, default_value_t = 0.95)]
    spike_amplitude: f64,
    /// Spike length, samples.
    #[arg(long, default_value_t = 256)]
    spike_samples: u64,
    /// Samples from the start of the spike to the retune. Must exceed
    /// `--spike-samples` so the spike is over before the band moves.
    #[arg(long, default_value_t = 1024)]
    gap_samples: u64,

    /// Number of swaps. 2 is one hop out and one hop back; raise it for repeats.
    #[arg(long, default_value_t = 2)]
    swaps: usize,
    /// Time parked on a band between swaps, ms. Also the tail kept after the
    /// last swap, and the window the analysis uses to establish what
    /// "recovered" looks like — which needs to stay comfortably longer than the
    /// outage it is measuring.
    #[arg(long, default_value_t = 10.0)]
    dwell_ms: f64,
    /// Tone-only time before the calibration spike, ms.
    #[arg(long, default_value_t = 50.0)]
    warmup_ms: f64,
    /// Time between the calibration spike and the first swap, ms. Has to cover
    /// finding the spike and scheduling the first pair of retunes — 60 ms to
    /// let the spike land, then a 40 ms scheduling lookahead, so 150 leaves
    /// room without padding the recording.
    #[arg(long, default_value_t = 150.0)]
    cal_wait_ms: f64,

    /// RX gain, dB, with manual gain control forced so it stays put. 0 dB, as
    /// in `retune_timing` and `freq_swap_reconfigure_streaming`.
    #[arg(long, default_value_t = 0)]
    rx_gain_db: i32,
    /// TX gain, dB. 40, as in `freq_sine_tx` and
    /// `freq_swap_reconfigure_streaming` — at 0 the tone does not survive the
    /// trip to the receiver and there is nothing in the recording to measure.
    #[arg(long, default_value_t = 40)]
    tx_gain_db: i32,
    /// Half-width of the window the calibration spike is looked for in, ms.
    /// Centred on where the two hardware counters say it should be.
    #[arg(long, default_value_t = 5.0)]
    cal_window_ms: f64,
    /// TX port, as labelled on the board: 1 or 2. The API index is one lower.
    #[arg(long, default_value_t = 2)]
    tx_port: u8,
    /// RX port, as labelled on the board: 1 or 2. The API index is one lower.
    #[arg(long, default_value_t = 1)]
    rx_port: u8,

    /// Detector window, samples. Sets how finely the recovery instant can be
    /// placed: the analysis steps a quarter of this at a time. 32 is 8 us of
    /// coherent integration at 4 MSps — fine against a ~150 us outage, and with
    /// plenty of margin over the noise at these gains.
    #[arg(long, default_value_t = 32)]
    win: usize,
    /// How much of the run before the carrier starts to keep in the file, ms.
    /// The transmit stream needs a long lead to come up, and several hundred
    /// milliseconds of pre-carrier noise makes the recording tiresome to read.
    /// The noise floor is measured on the untrimmed capture either way.
    #[arg(long, default_value_t = 5.0)]
    trim_ms: f64,
    /// IQ output path: interleaved int16, I first.
    #[arg(long, default_value = "quicktune_swap.sc16")]
    iq: String,
    /// Sidecar JSON with the sample rate, the event timestamps and the results.
    #[arg(long, default_value = "quicktune_swap.meta.json")]
    meta: String,
}

/// How far ahead of the clock the transmit burst opens, ms.
///
/// The first `bladerf_sync_tx` is what starts the transmit stream, and that
/// takes a few hundred milliseconds. Open the burst any nearer than that and
/// its timestamp is in the past before the FPGA ever sees it.
const LEAD_MS: f64 = 500.0;

fn ck(res: i32, what: &str) -> Result<()> {
    if res != 0 {
        bail!("{what} failed: {res}");
    }
    Ok(())
}

/// The board labels its ports TX1/TX2 and RX1/RX2; the API counts from zero.
fn tx_channel(port: u8) -> Result<Channel> {
    match port {
        1 => Ok(Channel::Tx0),
        2 => Ok(Channel::Tx1),
        _ => bail!("--tx-port must be 1 or 2, got {port}"),
    }
}

fn rx_channel(port: u8) -> Result<Channel> {
    match port {
        1 => Ok(Channel::Rx0),
        2 => Ok(Channel::Rx1),
        _ => bail!("--rx-port must be 1 or 2, got {port}"),
    }
}

/// `bladerf_quick_tune` is a union with a bladeRF1 arm and a bladeRF2 arm, and
/// the wrapper's `QuickTune` hardcodes the bladeRF1 one — which on a 2.0 micro
/// reads and writes the wrong fields. Always hand libbladeRF the full union so
/// it cannot write past what it was given. Same reasoning as `quick_tune_probe`.
fn zeroed_quick_tune() -> bladerf_quick_tune {
    // SAFETY: the union is plain old data; all-zero is a valid bit pattern.
    unsafe { std::mem::zeroed() }
}

/// One RX read, placed on the absolute RX timestamp axis.
struct Block {
    ts: u64,
    off: usize,
    len: usize,
}

#[derive(Default)]
struct Capture {
    samples: Vec<ComplexI16>,
    blocks: Vec<Block>,
    overruns: u64,
    errors: u64,
    /// libbladeRF's code for the first failed read, for the report.
    first_err: i32,
    /// Times the reader had to give up on the contiguous timestamp and ask for
    /// whatever is current instead. Each one is a hole in the recording.
    resyncs: u64,
}

/// A swap: what it moves between, and the transmit timestamps that define it.
struct Event {
    from_hz: u64,
    to_hz: u64,
    /// Index into the per-band profile arrays for the destination band.
    to_band: usize,
    /// TX timestamp at which the spike starts.
    spike_tx: u64,
    /// TX timestamp at which both retunes are scheduled to fire.
    retune_tx: u64,
}

/// What the analysis made of one event.
struct Measured {
    /// Where the spike was actually found, on the recording's timestamp axis.
    spike_rx: u64,
    /// Predicted position, from the calibration offset. The difference between
    /// this and `spike_rx` is how much the clock tie has drifted.
    spike_rx_predicted: u64,
    retune_rx: u64,
    /// First sample after the retune at which the tone is back, or None if it
    /// never came back within the dwell.
    recovery_rx: Option<u64>,
    /// First sample after the retune at which the tone had gone. Should sit
    /// essentially on the retune instant; if it does not, the alignment is off.
    outage_rx: Option<u64>,
    /// Where the spike search landed relative to the prediction, in samples.
    /// Zero on a healthy link; anything else says the edge could not be placed.
    drift: i64,
    /// Steady tone level before the swap and after it, linear.
    level_before: f64,
    level_after: f64,
    /// The destination band is as quiet as the receiver's own noise, so there
    /// is no tone there to come back and nothing to time. Distinguished from a
    /// slow recovery because the two look identical right up to the moment you
    /// divide by a threshold of zero and call the answer 0 us.
    dest_silent: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.gap_samples <= args.spike_samples {
        bail!(
            "--gap-samples ({}) must exceed --spike-samples ({}), or the band moves \
             while the spike is still being transmitted",
            args.gap_samples,
            args.spike_samples
        );
    }
    if args.swaps == 0 {
        bail!("--swaps must be at least 1");
    }
    let fs = args.sample_rate;
    let nyq = fs / 2.0;
    // Where the carrier lands in the receiver's baseband: the LO gap plus any
    // baseband offset. Everything downstream — the detector, the plot, the
    // spike search — keys on this one number.
    let detect_hz = args.tx_offset_hz + args.tone_hz;
    if detect_hz.abs() >= 0.95 * nyq {
        bail!(
            "the carrier lands at {:+.3} MHz in the receiver (--tx-offset-hz {:.3} plus \
             --tone-hz {:.3}), outside Nyquist (+/-{:.3} MHz at {:.3} MSps) — it would alias",
            detect_hz / 1e6,
            args.tx_offset_hz / 1e6,
            args.tone_hz / 1e6,
            nyq / 1e6,
            fs / 1e6
        );
    }

    let tx_ch = tx_channel(args.tx_port)?;
    let rx_ch = rx_channel(args.rx_port)?;
    let tx_raw = tx_ch as bladerf_channel;
    let rx_raw = rx_ch as bladerf_channel;

    // ── Device setup ─────────────────────────────────────────────────────────
    let dev = Arc::new(BladeRfAny::open_first().context("cannot open bladeRF")?);
    println!("serial {}", dev.get_serial().unwrap_or_default());
    let ptr = dev.get_device_ptr();

    // Scheduled retunes are an FPGA feature: the NIOS holds the queue and fires
    // each entry when the sample counter reaches it. Host tuning cannot do it.
    dev.set_tuning_mode(TuningMode::FPGA)
        .context("set FPGA tuning mode")?;

    let rate = dev.set_sample_rate(rx_ch, fs as u32).context("rx rate")?;
    dev.set_sample_rate(tx_ch, fs as u32).context("tx rate")?;
    let bw = args.bandwidth.unwrap_or(fs);
    let bw_actual = dev.set_bandwidth(rx_ch, bw as u32).context("rx bandwidth")?;
    dev.set_bandwidth(tx_ch, bw as u32).context("tx bandwidth")?;

    println!(
        "TX{} (api {:?}) -> RX{} (api {:?}), {:.3} MSps, {:.3} MHz bandwidth, tuning {:?}",
        args.tx_port,
        tx_ch,
        args.rx_port,
        rx_ch,
        rate as f64 / 1e6,
        bw_actual as f64 / 1e6,
        dev.get_tuning_mode()?,
    );

    // Metadata format on both directions. The wrapper's streamers cannot ask
    // for it — `Format::Sc16Q11Meta` is commented out in the wrapper — and
    // without it neither the burst timestamp nor the recording's time axis
    // exists, so this drops to the sync C API for both.
    let (nbuf, bufsz, ntrans, timeout_ms) = (64u32, 16384u32, 16u32, 3500u32);
    // SAFETY: live device; buffer size is the required multiple of 1024 and
    // num_buffers > num_transfers, which is what libbladeRF validates.
    unsafe {
        ck(
            bladerf_sync_config(
                ptr,
                bladerf_channel_layout_BLADERF_RX_X1,
                bladerf_format_BLADERF_FORMAT_SC16_Q11_META,
                nbuf,
                bufsz,
                ntrans,
                timeout_ms,
            ),
            "bladerf_sync_config(RX)",
        )?;
        ck(
            bladerf_sync_config(
                ptr,
                bladerf_channel_layout_BLADERF_TX_X1,
                bladerf_format_BLADERF_FORMAT_SC16_Q11_META,
                nbuf,
                bufsz,
                ntrans,
                timeout_ms,
            ),
            "bladerf_sync_config(TX)",
        )?;
    }

    let tx_hz = |rx_hz: u64| rx_hz + args.tx_offset_hz as u64;
    dev.set_frequency(rx_ch, args.freq_a).context("rx freq A")?;
    dev.set_frequency(tx_ch, tx_hz(args.freq_a)).context("tx freq A")?;
    dev.set_enable_module(rx_ch, true).context("enable RX")?;
    dev.set_enable_module(tx_ch, true).context("enable TX")?;

    // ── Capture the quick-tune profiles ──────────────────────────────────────
    // One per band per direction, with the radio already sitting on the
    // frequency being captured — that is libbladeRF's precondition. Four total,
    // nowhere near the NIOS slot ceiling that bites `retune_matrix_brf`.
    let bands = [("A", args.freq_a), ("B", args.freq_b)];
    let mut rx_qt = Vec::new();
    let mut tx_qt = Vec::new();
    for (label, hz) in bands {
        dev.set_frequency(rx_ch, hz)?;
        dev.set_frequency(tx_ch, tx_hz(hz))?;
        thread::sleep(Duration::from_millis(50));
        let mut r = zeroed_quick_tune();
        let mut t = zeroed_quick_tune();
        // SAFETY: live device, full union type for both arms.
        unsafe {
            ck(
                bladerf_get_quick_tune(ptr, rx_raw, &mut r),
                &format!("get_quick_tune(RX {label})"),
            )?;
            ck(
                bladerf_get_quick_tune(ptr, tx_raw, &mut t),
                &format!("get_quick_tune(TX {label})"),
            )?;
        }
        println!(
            "  captured band {label}: RX {:9.3} MHz, TX {:9.3} MHz",
            hz as f64 / 1e6,
            tx_hz(hz) as f64 / 1e6
        );
        rx_qt.push(r);
        tx_qt.push(t);
    }
    // Back to band A, which is where the run starts.
    dev.set_frequency(rx_ch, args.freq_a)?;
    dev.set_frequency(tx_ch, tx_hz(args.freq_a))?;
    thread::sleep(Duration::from_millis(50));

    // 0 dB everywhere, and manual on the receiver so it stays there. This has
    // to come *after* the profile capture: retuning re-applies the AD9361 gain
    // table, so a gain set before those four `set_frequency` calls is back at
    // the device default by the time the run starts — which is exactly what
    // happened until this moved down here.
    dev.set_gain_mode(rx_ch, GainMode::Manual)
        .context("set RX manual gain control")?;
    dev.set_gain(rx_ch, args.rx_gain_db).context("set RX gain")?;
    dev.set_gain(tx_ch, args.tx_gain_db).context("set TX gain")?;
    let (g_rx, g_tx) = (dev.get_gain(rx_ch)?, dev.get_gain(tx_ch)?);
    println!(
        "gain: RX {g_rx} dB ({:?}), TX {g_tx} dB",
        dev.get_gain_mode(rx_ch)?
    );
    if g_rx != args.rx_gain_db || g_tx != args.tx_gain_db {
        eprintln!(
            "WARNING: gain did not take — asked for RX {} / TX {}, device is holding RX {g_rx} / \
             TX {g_tx} dB",
            args.rx_gain_db, args.tx_gain_db
        );
    }

    let ms = |x: f64| (x * 1e-3 * fs) as u64;

    // ── Receiver: capture everything, timestamped ────────────────────────────
    // This runs before the schedule is built on purpose. Starting the RX stream
    // takes a few hundred milliseconds, and the ring fills while it happens, so
    // a timeline pinned to the clock beforehand would have its first quarter
    // second already gone by the time the first sample is kept — calibration
    // spike included.
    let stop = Arc::new(AtomicBool::new(false));
    let rx_ready = Arc::new(AtomicBool::new(false));
    // Room for the whole run plus the lead-in and a margin, so the recorder
    // never reallocates mid-capture and never grows without bound.
    let cap_samples = ms(LEAD_MS
        + args.warmup_ms
        + args.cal_wait_ms
        + args.swaps as f64 * args.dwell_ms
        + 400.0) as usize;
    let capture = Arc::new(Mutex::new(Capture {
        samples: Vec::with_capacity(cap_samples),
        ..Default::default()
    }));

    let rx_thread = {
        let (dev, stop, capture, ready) =
            (dev.clone(), stop.clone(), capture.clone(), rx_ready.clone());
        let block = 16384usize;
        thread::spawn(move || {
            let ptr = dev.get_device_ptr();
            let mut buf = vec![ComplexI16::new(0, 0); block];

            // Catch up to the present before keeping anything. The first
            // sync_rx is what actually starts the stream, and what it returns
            // is already stale by the width of that start-up — ask for the
            // sample after it and libbladeRF answers TIME_PAST.
            while !stop.load(Ordering::Relaxed) {
                // SAFETY: live device configured for RX_X1 + SC16_Q11_META
                // above; `buf` holds `block` samples of the matching type.
                let mut meta: bladerf_metadata = unsafe { std::mem::zeroed() };
                meta.flags = BLADERF_META_FLAG_RX_NOW;
                let res = unsafe {
                    bladerf_sync_rx(ptr, buf.as_mut_ptr().cast(), block as u32, &mut meta, 2000)
                };
                if res != 0 {
                    continue;
                }
                let mut now: u64 = 0;
                // SAFETY: live device, RX enabled and counting.
                if unsafe { bladerf_get_timestamp(ptr, bladerf_direction_BLADERF_RX, &mut now) } != 0
                {
                    break;
                }
                // Level when the reader is no more than a couple of buffers
                // behind what the FPGA has already counted.
                if now.saturating_sub(meta.timestamp + block as u64) <= 2 * block as u64 {
                    break;
                }
            }
            ready.store(true, Ordering::Release);

            // From here the reads are contiguous: each one names the timestamp
            // it wants, which is the one right after the last sample kept. That
            // is what makes the recording gapless — RX_NOW returns whatever
            // happens to be current and silently drops the rest.
            let mut next_ts: Option<u64> = None;
            while !stop.load(Ordering::Relaxed) {
                // SAFETY: as above.
                let mut meta: bladerf_metadata = unsafe { std::mem::zeroed() };
                match next_ts {
                    None => meta.flags = BLADERF_META_FLAG_RX_NOW,
                    Some(ts) => meta.timestamp = ts,
                }
                let res = unsafe {
                    bladerf_sync_rx(
                        ptr,
                        buf.as_mut_ptr().cast(),
                        block as u32,
                        &mut meta,
                        2000,
                    )
                };
                let mut cap = capture.lock().unwrap();
                if res != 0 {
                    cap.errors += 1;
                    if cap.first_err == 0 {
                        cap.first_err = res;
                    }
                    // The requested range is gone for good — asking again for
                    // the same timestamp fails identically, forever. Drop back
                    // to "whatever is current" and let the block index record
                    // the hole.
                    if next_ts.take().is_some() {
                        cap.resyncs += 1;
                    }
                    continue;
                }
                let n = match meta.actual_count as usize {
                    0 => block,
                    n if n <= block => n,
                    _ => block,
                };
                if meta.status & BLADERF_META_STATUS_OVERRUN != 0 {
                    cap.overruns += 1;
                }
                // Stop rather than realloc: a reallocation of a 100 MB buffer
                // mid-capture is exactly how an overrun gets manufactured.
                if cap.samples.len() + n > cap.samples.capacity() {
                    break;
                }
                let off = cap.samples.len();
                cap.samples.extend_from_slice(&buf[..n]);
                cap.blocks.push(Block { ts: meta.timestamp, off, len: n });
                next_ts = Some(meta.timestamp + n as u64);
            }
        })
    };

    // ── Build the schedule, in TX timestamps ─────────────────────────────────
    // Wait for the receiver before reading the clock, so the schedule is built
    // against a stream that is already flowing.
    let t_wait = Instant::now();
    while !rx_ready.load(Ordering::Acquire) {
        if t_wait.elapsed() > Duration::from_secs(10) {
            stop.store(true, Ordering::Relaxed);
            bail!("receiver never reached the live sample clock");
        }
        thread::sleep(Duration::from_millis(2));
    }
    println!("receiver settled in {:.0} ms", t_wait.elapsed().as_secs_f64() * 1e3);

    let lead = ms(LEAD_MS);
    let (mut tx_now, mut rx_now) = (0u64, 0u64);
    // SAFETY: live device; both counters have been running since their modules
    // were enabled. Read back to back, so the difference is wrong only by
    // whatever passed between the two round trips.
    unsafe {
        ck(
            bladerf_get_timestamp(ptr, bladerf_direction_BLADERF_TX, &mut tx_now),
            "bladerf_get_timestamp(TX)",
        )?;
        ck(
            bladerf_get_timestamp(ptr, bladerf_direction_BLADERF_RX, &mut rx_now),
            "bladerf_get_timestamp(RX)",
        )?;
    }
    // Wall-clock zero, pinned to the sample clock that was just read. Every
    // wait below is measured from here, so `tx_now` and this instant are the
    // same moment in the two clocks.
    let t_clock = Instant::now();
    // Good to a USB round trip — far too coarse to time a retune with, but
    // easily good enough to say which millisecond of the recording the
    // calibration spike is in, which is all it is used for.
    let coarse_delta = rx_now as i64 - tx_now as i64;
    let t0 = tx_now + lead;
    let cal_tx = t0 + ms(args.warmup_ms);
    let first_tx = cal_tx + ms(args.cal_wait_ms);
    let dwell = ms(args.dwell_ms);

    let mut events = Vec::with_capacity(args.swaps);
    for k in 0..args.swaps {
        // Starts on A, so even hops go to B and odd hops come back to A.
        let (from_hz, to_hz, to_band) = if k % 2 == 0 {
            (args.freq_a, args.freq_b, 1usize)
        } else {
            (args.freq_b, args.freq_a, 0usize)
        };
        let spike_tx = first_tx + k as u64 * dwell;
        events.push(Event {
            from_hz,
            to_hz,
            to_band,
            spike_tx,
            retune_tx: spike_tx + args.gap_samples,
        });
    }
    let end_tx = events.last().unwrap().spike_tx + dwell;
    let total_tx = end_tx - t0;

    // Every spike the transmitter has to place, calibration first.
    let mut spikes: Vec<(u64, u64)> = vec![(cal_tx, cal_tx + args.spike_samples)];
    spikes.extend(
        events
            .iter()
            .map(|e| (e.spike_tx, e.spike_tx + args.spike_samples)),
    );

    println!(
        "\n{} swap{} between {:.3} and {:.3} MHz, dwell {:.0} ms, spike {} samples, \
         retune {} samples after each spike",
        args.swaps,
        if args.swaps == 1 { "" } else { "s" },
        args.freq_a as f64 / 1e6,
        args.freq_b as f64 / 1e6,
        args.dwell_ms,
        args.spike_samples,
        args.gap_samples,
    );
    println!(
        "  RX {:.3} / TX {:.3} MHz, then RX {:.3} / TX {:.3} MHz — carrier lands at {:+.3} MHz \
         in the receiver",
        args.freq_a as f64 / 1e6,
        tx_hz(args.freq_a) as f64 / 1e6,
        args.freq_b as f64 / 1e6,
        tx_hz(args.freq_b) as f64 / 1e6,
        detect_hz / 1e6,
    );
    println!(
        "  carrier at {:.2} FS, spike at {:.2} FS, capture {:.3} s",
        args.amplitude,
        args.spike_amplitude,
        total_tx as f64 / fs,
    );

    // ── Transmitter: tone, with spikes at the scheduled timestamps ───────────
    let tx_fault: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tx_thread = {
        let (dev, spikes, fault) = (dev.clone(), spikes.clone(), tx_fault.clone());
        let (amp, spike_amp) = (
            args.amplitude * 2047.0,
            args.spike_amplitude * 2047.0,
        );
        let cyc = args.tone_hz / fs;
        let block = 16384usize;
        thread::spawn(move || -> Result<()> {
            let ptr = dev.get_device_ptr();
            let mut buf = vec![ComplexI16::new(0, 0); block];
            let mut written: u64 = 0;
            let mut first = true;
            let mut underruns: u64 = 0;
            while written < total_tx {
                let n = block.min((total_tx - written) as usize);
                let first_ts = t0 + written;
                fill_tone(&mut buf[..n], first_ts, t0, cyc, amp);
                // Overwrite whatever part of a spike falls in this block. The
                // phase is recomputed rather than scaled, so the spike is the
                // same tone, just loud — it stays in band and survives the
                // receiver's filter as cleanly as the tone does.
                for &(a, b) in &spikes {
                    let lo = a.max(first_ts);
                    let hi = b.min(first_ts + n as u64);
                    if lo < hi {
                        let k = (lo - first_ts) as usize;
                        let len = (hi - lo) as usize;
                        fill_tone(&mut buf[k..k + len], lo, t0, cyc, spike_amp);
                    }
                }

                // SAFETY: live device configured for TX_X1 + SC16_Q11_META.
                let mut meta: bladerf_metadata = unsafe { std::mem::zeroed() };
                if first {
                    // Open the burst at an absolute timestamp. Everything after
                    // continues it contiguously, so sample k of the burst is at
                    // timestamp t0 + k by construction — which is the whole
                    // reason the spike can be a clock reference at all.
                    meta.flags = BLADERF_META_FLAG_TX_BURST_START;
                    meta.timestamp = t0;
                    first = false;
                }
                let res = unsafe {
                    bladerf_sync_tx(ptr, buf.as_ptr().cast(), n as u32, &mut meta, 3000)
                };
                if res != 0 {
                    // Publish it now rather than at join time: the calibration
                    // step is about to run, and "no spike" from a transmitter
                    // that never started reads as "no coupling" otherwise.
                    *fault.lock().unwrap() =
                        Some(format!("bladerf_sync_tx failed: {res} (after {written} samples)"));
                    bail!("bladerf_sync_tx failed: {res}");
                }
                if meta.status & BLADERF_META_STATUS_UNDERRUN != 0 {
                    underruns += 1;
                }
                written += n as u64;
            }
            // SAFETY: as above; a zero-length write with BURST_END closes it.
            let mut meta: bladerf_metadata = unsafe { std::mem::zeroed() };
            meta.flags = BLADERF_META_FLAG_TX_BURST_END;
            unsafe {
                bladerf_sync_tx(ptr, buf.as_ptr().cast(), 0, &mut meta, 3000);
            }
            if underruns > 0 {
                eprintln!(
                    "WARNING: {underruns} TX underruns — the tone has holes in it, and a hole \
                     near a swap will read as an outage"
                );
            }
            Ok(())
        })
    };

    // ── Calibrate the RX/TX clock offset from the first spike ────────────────
    // Wall clock is only used to decide when to look; every number that ends up
    // in the result comes from sample timestamps.
    let wall = |tx_ts: u64| Duration::from_secs_f64(tx_ts.saturating_sub(tx_now) as f64 / fs);
    sleep_until(t_clock, wall(cal_tx) + Duration::from_millis(60));

    // Precise if the spike can be found, coarse from the two counters if it
    // cannot. Coarse is worth having: it still puts real swaps into the
    // recording, which is the thing worth looking at, and it says so loudly
    // rather than quietly reporting a latency it has no right to.
    let (delta, cal_rx, precise) = {
        let (ts0, iq) = {
            let cap = capture.lock().unwrap();
            assemble(&cap)
        };
        if iq.is_empty() {
            stop.store(true, Ordering::Relaxed);
            bail!("no samples captured — is the receiver streaming?");
        }
        if let Some(e) = tx_fault.lock().unwrap().as_ref() {
            stop.store(true, Ordering::Relaxed);
            bail!("transmitter never got going: {e}");
        }

        let dbfs = |lin: f64| 20.0 * (lin / 2048.0).max(1e-12).log10();
        let tone_from = ((cal_tx as i64 + coarse_delta) - ts0 as i64).max(0) as usize;
        let tone = level(
            &iq,
            (tone_from + ms(5.0) as usize).min(iq.len()),
            (tone_from + ms(30.0) as usize).min(iq.len()),
            args.win,
            detect_hz / fs,
        );
        // Search only around where the hardware counters put the spike. Across
        // the whole recording the loudest thing is the stream's own start-up
        // transient in the first block, and latching onto that yields a
        // confident, precise, entirely wrong clock offset.
        let centre = (cal_tx as i64 + coarse_delta) - ts0 as i64;
        let half = ms(args.cal_window_ms) as i64;
        let lo = centre.saturating_sub(half).max(0) as usize;
        let hi = ((centre + half).max(0) as usize).min(iq.len());
        let found = if lo < hi {
            find_spike(&iq[lo..hi], detect_hz / fs, args.win).map(|(p, pk, fl)| (p + lo, pk, fl))
        } else {
            None
        };
        match &found {
            Some((_, peak, floor)) => println!(
                "\nreceiver sees: tone {:.1} dBFS, broadband {:.1} dBFS, loudest {:.1} dBFS",
                dbfs(tone),
                dbfs(*floor),
                dbfs(*peak),
            ),
            None => println!("\nreceiver sees: tone {:.1} dBFS, nothing else", dbfs(tone)),
        }

        match found {
            Some((pos, peak, floor)) if peak >= 2.0 * floor.max(1e-9) => {
                let cal_rx = ts0 + pos as u64;
                println!(
                    "calibration spike at RX ts {cal_rx}, {:.1}x above the tone; \
                     RX-TX clock offset {} samples ({:+.3} ms)",
                    peak / floor.max(1e-9),
                    cal_rx as i64 - cal_tx as i64,
                    (cal_rx as i64 - cal_tx as i64) as f64 / fs * 1e3,
                );
                (cal_rx as i64 - cal_tx as i64, Some(cal_rx), true)
            }
            other => {
                let margin = other
                    .map(|(_, p, f)| p / f.max(1e-9))
                    .unwrap_or(0.0);
                let coarse = coarse_delta;
                eprintln!(
                    "WARNING: no calibration spike ({margin:.2}x above the noise, need 2x). With \
                     every gain stage at 0 dB the link wants a cable between TX{} and RX{}; over \
                     the air there is nothing to hear.\n\
                     \x20        Falling back to the RX/TX counter difference, {coarse} samples. \
                     That is only good to a USB round trip, so the swaps will land in the \
                     recording but no latency will be reported. The IQ is still written.",
                    args.tx_port, args.rx_port,
                );
                (coarse, None, false)
            }
        }
    };

    // ── Schedule the swaps ───────────────────────────────────────────────────
    // A few at a time, staying ahead of the sample counter rather than dumping
    // the lot into the NIOS queue, which is short and will refuse a flood.
    let lookahead = Duration::from_millis(40);
    for (k, e) in events.iter().enumerate() {
        let due = wall(e.retune_tx);
        sleep_until(t_clock, due.saturating_sub(lookahead));
        let rx_at = (e.retune_tx as i64 + delta) as u64;
        // SAFETY: live device; the profiles are the full union type, captured
        // from this device above, and the timestamps are still in the future.
        unsafe {
            ck(
                bladerf_schedule_retune(
                    ptr,
                    tx_raw,
                    e.retune_tx,
                    tx_hz(e.to_hz),
                    &mut tx_qt[e.to_band],
                ),
                &format!("schedule TX retune #{k}"),
            )?;
            ck(
                bladerf_schedule_retune(ptr, rx_raw, rx_at, e.to_hz, &mut rx_qt[e.to_band]),
                &format!("schedule RX retune #{k}"),
            )?;
        }
        println!(
            "  swap {}/{}: {:8.3} -> {:8.3} MHz, TX ts {} / RX ts {}",
            k + 1,
            args.swaps,
            e.from_hz as f64 / 1e6,
            e.to_hz as f64 / 1e6,
            e.retune_tx,
            rx_at,
        );
    }

    // ── Let it finish ────────────────────────────────────────────────────────
    sleep_until(t_clock, wall(end_tx) + Duration::from_millis(120));
    stop.store(true, Ordering::Relaxed);
    let _ = rx_thread.join();
    match tx_thread.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("TX thread failed: {e}"),
        Err(_) => eprintln!("TX thread panicked"),
    }

    let _ = dev.cancel_scheduled_retune(tx_ch);
    let _ = dev.cancel_scheduled_retune(rx_ch);
    let _ = dev.set_enable_module(tx_ch, false);
    let _ = dev.set_enable_module(rx_ch, false);
    let _ = dev.set_frequency(rx_ch, args.freq_a);
    let _ = dev.set_frequency(tx_ch, tx_hz(args.freq_a));

    // ── Analyse ──────────────────────────────────────────────────────────────
    let cap = capture.lock().unwrap();
    let (ts0, iq) = assemble(&cap);
    let (overruns, errors) = (cap.overruns, cap.errors);
    let (first_err, resyncs) = (cap.first_err, cap.resyncs);
    let gaps = count_gaps(&cap);
    if std::env::var_os("QTSWAP_DEBUG").is_some() {
        eprintln!("DEBUG first blocks (ts, len):");
        for b in cap.blocks.iter().take(6) {
            eprintln!("  ts {:>12} len {}", b.ts, b.len);
        }
        let mut expect: Option<u64> = None;
        for (i, b) in cap.blocks.iter().enumerate() {
            if let Some(e) = expect {
                if b.ts > e {
                    eprintln!(
                        "DEBUG gap before block {i}: {} samples, at ts {e} ({:.1} ms in)",
                        b.ts - e,
                        (e - cap.blocks[0].ts) as f64 / fs * 1e3
                    );
                }
            }
            expect = Some(b.ts + b.len as u64);
        }
    }
    drop(cap);

    if overruns > 0 || errors > 0 || gaps > 0 {
        eprintln!(
            "WARNING: {overruns} RX overruns, {errors} read errors (first code {first_err}), \
             {resyncs} resyncs, {gaps} zero-filled gaps — a gap near a swap invalidates that \
             swap's number"
        );
    }
    println!(
        "\ncaptured {} samples ({:.3} s), RX ts {}..{}",
        iq.len(),
        iq.len() as f64 / fs,
        ts0,
        ts0 + iq.len() as u64,
    );

    let cyc = detect_hz / fs;
    // What the detector reads when there is definitely no tone: the stretch
    // before the burst opens. Everything else is judged against this, so
    // "quiet" means quiet by the receiver's own standard rather than by a
    // constant guessed in advance.
    let pre_end = ((t0 as i64 + delta) - ts0 as i64 - ms(10.0) as i64).max(0) as usize;
    let noise = level(&iq, 0, pre_end.min(iq.len()), args.win, cyc);
    let dbfs = |lin: f64| 20.0 * (lin / 2048.0).max(1e-12).log10();
    println!("detector noise floor {:.1} dBFS", dbfs(noise));

    let measured: Vec<Option<Measured>> = if precise {
        events
            .iter()
            .map(|e| measure(e, &iq, ts0, delta, cyc, args.win, dwell, args.spike_samples, noise))
            .collect()
    } else {
        events.iter().map(|_| None).collect()
    };

    println!("\non-air quick-tune latency, retune fired -> tone back:");
    let mut good = Vec::new();
    for (k, (e, m)) in events.iter().zip(&measured).enumerate() {
        let hop = format!(
            "{:7.1} -> {:7.1} MHz",
            e.from_hz as f64 / 1e6,
            e.to_hz as f64 / 1e6
        );
        match m {
            Some(m) => {
                let drift = m.drift;
                let levels = format!(
                    "{:.1} -> {:.1} dBFS",
                    dbfs(m.level_before),
                    dbfs(m.level_after)
                );
                let drop_us = m
                    .outage_rx
                    .map(|o| {
                        format!("{:+.1}", (o as i64 - m.retune_rx as i64) as f64 / fs * 1e6)
                    })
                    .unwrap_or_else(|| "-".into());
                // A drop that predates the retune means the anchor is wrong or
                // the retune fired early; either way the interval below is not
                // measuring a scheduled swap.
                let early = m.outage_rx.is_some_and(|o| o < m.retune_rx);
                // The clock tie is exact when the link is healthy, so a spike
                // that has wandered is the signal that this band is too weak
                // for the edge to be placed properly.
                let shaky = drift.abs() > (args.spike_samples / 4) as i64;
                match m.recovery_rx {
                    Some(r) => {
                        let us = (r - m.retune_rx) as f64 / fs * 1e6;
                        let flag = match (early, shaky) {
                            (false, false) => {
                                good.push(us);
                                ""
                            }
                            (true, _) => "  SUSPECT: tone dropped before the retune",
                            (_, true) => "  SUSPECT: spike drifted",
                        };
                        println!(
                            "  swap {:>2}  {hop}   {us:9.1} us   (old band gone at {drop_us} us)  \
                             {levels}   drift {drift:+}{flag}",
                            k + 1,
                        );
                    }
                    None if m.dest_silent => println!(
                        "  swap {:>2}  {hop}   destination band is silent ({levels}) — no tone \
                         arrives there to time",
                        k + 1
                    ),
                    None if m.outage_rx.is_none() => println!(
                        "  swap {:>2}  {hop}   the tone never dropped ({levels}) — the retune did \
                         not fire, or it fired somewhere other than where it was scheduled",
                        k + 1
                    ),
                    None => println!(
                        "  swap {:>2}  {hop}   tone never came back within the dwell ({levels}, \
                         old band gone at {drop_us} us)",
                        k + 1
                    ),
                }
            }
            None => println!("  swap {:>2}  {hop}   could not locate the spike", k + 1),
        }
    }
    if !good.is_empty() {
        good.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "\n  n={}  min {:.1} us   median {:.1} us   max {:.1} us",
            good.len(),
            good[0],
            good[good.len() / 2],
            good[good.len() - 1],
        );
    }

    // Everything measured is done; the file only has to be readable. Drop the
    // lead-in the transmit stream needed to come up — it is several times
    // longer than the part anyone wants to look at. Timestamps are absolute, so
    // the sidecar's `rx_ts0` moves with the trim and every event stays where it
    // says it is.
    let trim = (((t0 as i64 + delta) - ts0 as i64) - ms(args.trim_ms) as i64)
        .max(0)
        .min(iq.len() as i64) as usize;
    // And the tail after the carrier stops, which is only the margin the
    // recorder was given to be sure it outlived the transmitter.
    let tail = (((end_tx as i64 + delta) - ts0 as i64) + ms(args.trim_ms) as i64)
        .max(trim as i64)
        .min(iq.len() as i64) as usize;
    let out = &iq[trim..tail];
    let out_ts0 = ts0 + trim as u64;
    write_iq(&args.iq, out)?;
    write_meta(
        &args, fs, detect_hz, out_ts0, delta, cal_rx, precise, &events, &measured, overruns,
        errors, gaps,
    )?;
    println!(
        "\nIQ:    {} — {} samples ({:.1} ms), interleaved int16 I first, sample 0 at RX ts {}",
        std::fs::canonicalize(&args.iq)
            .unwrap_or_else(|_| args.iq.clone().into())
            .display(),
        out.len(),
        out.len() as f64 / fs * 1e3,
        out_ts0,
    );
    println!(
        "meta:  {} — event timestamps, all on the same RX axis as the IQ",
        std::fs::canonicalize(&args.meta)
            .unwrap_or_else(|_| args.meta.clone().into())
            .display(),
    );
    println!("plot:  python3 plot_quicktune_swap_iq.py {} {}", args.iq, args.meta);
    Ok(())
}

/// Sleep until `d` has elapsed since `start`. Returns immediately if it already has.
fn sleep_until(start: Instant, d: Duration) {
    let elapsed = start.elapsed();
    if d > elapsed {
        thread::sleep(d - elapsed);
    }
}

/// Fill `buf` with the tone, where `buf[0]` is at timestamp `first_ts`.
///
/// The phase comes from the absolute sample index rather than from a running
/// accumulator, so it is exact at any point in a multi-second run and identical
/// across blocks — no drift to explain away later.
fn fill_tone(buf: &mut [ComplexI16], first_ts: u64, t0: u64, cyc: f64, amp: f64) {
    for (i, s) in buf.iter_mut().enumerate() {
        let n = (first_ts + i as u64 - t0) as f64;
        let ph = (n * cyc).fract() * std::f64::consts::TAU;
        s.re = (amp * ph.cos()) as i16;
        s.im = (amp * ph.sin()) as i16;
    }
}

/// Lay the captured blocks out on the absolute RX timestamp axis, zero-filling
/// anything the receiver missed, and return the timestamp of sample 0.
///
/// This is what makes the recording directly indexable by time: after this,
/// sample `i` is at RX timestamp `ts0 + i`, with no block bookkeeping left.
fn assemble(cap: &Capture) -> (u64, Vec<ComplexI16>) {
    let Some(first) = cap.blocks.first() else {
        return (0, Vec::new());
    };
    let ts0 = first.ts;
    let last = cap.blocks.last().unwrap();
    let end = last.ts + last.len as u64;
    if end <= ts0 {
        return (ts0, Vec::new());
    }
    let mut out = vec![ComplexI16::new(0, 0); (end - ts0) as usize];
    for b in &cap.blocks {
        if b.ts < ts0 {
            continue;
        }
        let i = (b.ts - ts0) as usize;
        if i + b.len > out.len() {
            continue;
        }
        out[i..i + b.len].copy_from_slice(&cap.samples[b.off..b.off + b.len]);
    }
    (ts0, out)
}

/// Samples the receiver never delivered — the holes `assemble` had to zero-fill.
fn count_gaps(cap: &Capture) -> u64 {
    let mut gaps = 0;
    let mut expect: Option<u64> = None;
    for b in &cap.blocks {
        if let Some(e) = expect {
            if b.ts > e {
                gaps += b.ts - e;
            }
        }
        expect = Some(b.ts + b.len as u64);
    }
    gaps
}

/// Find the spike: the loudest thing the tone detector hears.
///
/// Returns its leading edge, its magnitude, and the typical magnitude
/// elsewhere in the same stretch. Deliberately threshold-free — with every gain
/// stage at 0 dB the received level is whatever the coupling gives, and is not
/// known in advance — so the search keys on the spike standing above its own
/// surroundings, and the caller checks that margin before believing it.
///
/// Detection is coherent, through the same mix-and-integrate detector used
/// everywhere else, rather than on raw magnitude. The spike is the tone at a
/// higher amplitude, so mixing it down concentrates it while spreading the
/// noise: on a raw envelope the spike on the weaker of the two bands sits
/// *below* the peaks of the receiver's own noise and cannot be found at all.
fn find_spike(iq: &[ComplexI16], cyc: f64, win: usize) -> Option<(usize, f64, f64)> {
    let hop = (win / 4).max(1);
    if iq.len() <= win {
        return None;
    }
    let env: Vec<(usize, f64)> = (0..iq.len() - win)
        .step_by(hop)
        .map(|i| (i, tone_mag(iq, i, win, cyc)))
        .collect();
    if env.len() < 8 {
        return None;
    }
    let (bi, (peak_pos, peak)) = env
        .iter()
        .cloned()
        .enumerate()
        .max_by(|a, b| a.1 .1.partial_cmp(&b.1 .1).unwrap())?;
    if peak <= 0.0 {
        return None;
    }

    // Typical level away from the spike, for the caller's sanity check. The
    // guard band is the spike's own width, so its shoulders do not raise the
    // floor it is being compared against.
    let guard = (win * 4) / hop + 1;
    let mut rest: Vec<f64> = env
        .iter()
        .enumerate()
        .filter(|(i, _)| i.abs_diff(bi) > guard)
        .map(|(_, (_, v))| *v)
        .collect();
    if rest.is_empty() {
        return None;
    }
    rest.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = rest[rest.len() / 2];

    // Leading edge, to a sample: walk forward from well before the peak to the
    // first place a short coherent window reaches half the spike's amplitude.
    // The same rule runs on the calibration spike and on every later one, so
    // the detector's rise time biases them all identically and cancels out of
    // every interval reported.
    let short = (win / 2).max(8);
    let from = peak_pos.saturating_sub(win * 8);
    let edge = (from..=peak_pos).find(|&i| tone_mag(iq, i, short, cyc) >= peak * 0.5)?;
    Some((edge, peak, floor))
}

/// Magnitude of the transmitted tone over one window, by mixing it down to DC
/// and integrating — a Goertzel at `cyc` cycles/sample.
///
/// Mixing rather than taking a plain envelope is what makes this robust: DC
/// offset, LO leakage and broadband noise all integrate away, so the detector
/// answers "is *our* tone here", not "is there energy here". During a retune the
/// two LOs disagree by hundreds of MHz, and the answer is unambiguously no.
fn tone_mag(iq: &[ComplexI16], start: usize, win: usize, cyc: f64) -> f64 {
    let end = (start + win).min(iq.len());
    if start >= end {
        return 0.0;
    }
    let (mut sr, mut si) = (0.0f64, 0.0f64);
    for (k, s) in iq[start..end].iter().enumerate() {
        let ph = -((start + k) as f64 * cyc).fract() * std::f64::consts::TAU;
        let (c, sn) = (ph.cos(), ph.sin());
        sr += s.re as f64 * c - s.im as f64 * sn;
        si += s.re as f64 * sn + s.im as f64 * c;
    }
    (sr * sr + si * si).sqrt() / (end - start) as f64
}

/// Median tone magnitude over a span, sampled at `win`-sized windows.
fn level(iq: &[ComplexI16], from: usize, to: usize, win: usize, cyc: f64) -> f64 {
    let to = to.min(iq.len());
    if from >= to {
        return 0.0;
    }
    let mut v: Vec<f64> = (from..to.saturating_sub(win))
        .step_by(win)
        .map(|i| tone_mag(iq, i, win, cyc))
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Work out, for one swap, when the tone went and when it came back.
#[allow(clippy::too_many_arguments)]
fn measure(
    e: &Event,
    iq: &[ComplexI16],
    ts0: u64,
    delta: i64,
    cyc: f64,
    win: usize,
    dwell: u64,
    spike_samples: u64,
    noise: f64,
) -> Option<Measured> {
    let to_pos = |ts: u64| -> Option<usize> {
        let p = ts as i64 - ts0 as i64;
        (p >= 0 && (p as usize) < iq.len()).then_some(p as usize)
    };

    // Where the clock offset says the spike should be, and where it actually is.
    // Searching a window around the prediction rather than the whole record
    // keeps a later spike from being confused with an earlier one.
    let predicted = (e.spike_tx as i64 + delta) as u64;
    let pred_pos = to_pos(predicted)?;
    let slack = (dwell / 4) as usize;
    let from = pred_pos.saturating_sub(slack);
    // Stop at the retune. The spike is transmitted `gap` samples ahead of it and
    // cannot be on the far side, so there is nothing to gain by looking there —
    // and a great deal to lose: past the retune the window holds the
    // *destination* band, and whenever that band is louder than the spike is on
    // the source band, the search locks onto steady tone tens of milliseconds
    // away and every instant derived from it moves with it. Which band is
    // louder is a property of the antennas, so without this the answer is right
    // in one hop direction and wrong in the other.
    let to = (pred_pos + (e.retune_tx - e.spike_tx) as usize).min(iq.len());
    let (rel, peak, floor) = find_spike(&iq[from..to], cyc, win)?;
    if peak < 2.0 * floor {
        return None;
    }
    let spike_pos = from + rel;
    let spike_rx = ts0 + spike_pos as u64;

    // The retune instant is fixed relative to the spike *as transmitted*, so
    // anchor it to the spike that was found rather than to the prediction. Any
    // slop in the clock tie then drops out of the interval being reported.
    let retune_pos = spike_pos + (e.retune_tx - e.spike_tx) as usize;
    let retune_rx = ts0 + retune_pos as u64;

    let hop = win / 4;
    let dwell = dwell as usize;

    // What the tone looked like before the swap, and what it settles to after.
    // Reading "after" from the far end of the new dwell rather than assuming the
    // two bands are equally loud is the point: they rarely are, and a recovery
    // threshold taken from the old band would fire early or never.
    let level_before = level(
        iq,
        spike_pos.saturating_sub(dwell / 2),
        spike_pos.saturating_sub(dwell / 20),
        win,
        cyc,
    );
    let after_lo = (retune_pos + dwell / 2).min(iq.len());
    let after_hi = (retune_pos + dwell * 9 / 10).min(iq.len());
    let level_after = level(iq, after_lo, after_hi, win, cyc);

    // Nothing audible on the destination band means there is no recovery to
    // find. Without this the threshold below is a fraction of zero, every
    // window clears it, and the very first sample after the retune reports as
    // the tone returning — a confident 0 us that measures nothing at all.
    // 6 dB over the noise, not more: the two bands can differ by 15 dB purely
    // through the antennas, and raising the receiver's gain to reach the weak
    // one lifts the noise floor with it. A fixed generous margin rejects a band
    // that is perfectly measurable and calls the hop untimeable.
    let dest_silent = level_after < 2.0 * noise;
    let search_end = (retune_pos + dwell).min(iq.len());

    // First, when the old band goes quiet. The retune is commanded at
    // `retune_pos`, but the tone already in flight takes a few tens of
    // microseconds to disappear from the recording, so this is a measurement,
    // not an assumption.
    let mut outage_pos = None;
    if level_before > 0.0 {
        let mut i = spike_pos + spike_samples as usize;
        while i + win < search_end {
            if tone_mag(iq, i, win, cyc) < level_before * 0.5 {
                outage_pos = Some(i);
                break;
            }
            i += hop;
        }
    }

    // Then, when the new band arrives — searched from the outage, not from the
    // retune. Searching from the retune returns immediately and always: the old
    // band's tone is still above any threshold there, so the very first window
    // "passes" and the answer comes out as zero.
    // Half the settled level, but never so close to the noise that the noise can
    // cross it on its own: on a weak destination band, half of "8 dB over the
    // floor" is 2 dB over the floor, which the floor reaches by itself.
    let thresh = (level_after * 0.5).max(noise * 1.8);
    let hold = 4;
    let mut recovery_rx = None;
    if !dest_silent {
        if let Some(start) = outage_pos {
            let mut i = start;
            while i + win * hold < search_end {
                if (0..hold).all(|k| tone_mag(iq, i + k * win, win, cyc) >= thresh) {
                    recovery_rx = Some(ts0 + i as u64);
                    break;
                }
                i += hop;
            }
        }
    }
    let outage_rx = outage_pos.map(|i| ts0 + i as u64);

    Some(Measured {
        spike_rx,
        spike_rx_predicted: predicted,
        drift: spike_rx as i64 - predicted as i64,
        retune_rx,
        recovery_rx,
        outage_rx,
        level_before,
        level_after,
        dest_silent,
    })
}

/// Interleaved int16, I first — exactly what came off the ADC, no conversion.
fn write_iq(path: &str, iq: &[ComplexI16]) -> Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    for s in iq {
        w.write_all(&s.re.to_le_bytes())?;
        w.write_all(&s.im.to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_meta(
    args: &Args,
    fs: f64,
    detect_hz: f64,
    ts0: u64,
    delta: i64,
    cal_rx: Option<u64>,
    precise: bool,
    events: &[Event],
    measured: &[Option<Measured>],
    overruns: u64,
    errors: u64,
    gaps: u64,
) -> Result<()> {
    let mut f = File::create(&args.meta)?;
    writeln!(f, "{{")?;
    writeln!(f, "  \"sample_rate_hz\": {fs},")?;
    // The one the analysis mixes at: where the carrier actually lands in the
    // receiver's baseband. The two it is made of are recorded beside it.
    writeln!(f, "  \"carrier_offset_hz\": {detect_hz},")?;
    writeln!(f, "  \"tx_offset_hz\": {},", args.tx_offset_hz)?;
    writeln!(f, "  \"tone_hz\": {},", args.tone_hz)?;
    writeln!(f, "  \"rx_freq_a_hz\": {},", args.freq_a)?;
    writeln!(f, "  \"rx_freq_b_hz\": {},", args.freq_b)?;
    writeln!(f, "  \"format\": \"interleaved int16, I first\",")?;
    writeln!(f, "  \"rx_ts0\": {ts0},")?;
    writeln!(f, "  \"rx_tx_offset_samples\": {delta},")?;
    writeln!(
        f,
        "  \"calibration_spike_rx_ts\": {},",
        cal_rx.map(|v| v.to_string()).unwrap_or_else(|| "null".into())
    )?;
    writeln!(f, "  \"clock_tie\": \"{}\",", if precise { "spike" } else { "counters (coarse)" })?;
    writeln!(f, "  \"spike_samples\": {},", args.spike_samples)?;
    writeln!(f, "  \"gap_samples\": {},", args.gap_samples)?;
    writeln!(f, "  \"detector_window\": {},", args.win)?;
    writeln!(f, "  \"rx_overruns\": {overruns},")?;
    writeln!(f, "  \"rx_read_errors\": {errors},")?;
    writeln!(f, "  \"rx_gap_samples\": {gaps},")?;
    writeln!(f, "  \"events\": [")?;
    for (k, (e, m)) in events.iter().zip(measured).enumerate() {
        let comma = if k + 1 == events.len() { "" } else { "," };
        match m {
            Some(m) => {
                let lat = m
                    .recovery_rx
                    .map(|r| format!("{:.3}", (r - m.retune_rx) as f64 / fs * 1e6))
                    .unwrap_or_else(|| "null".into());
                writeln!(
                    f,
                    "    {{\"from_hz\": {}, \"to_hz\": {}, \"spike_rx_ts\": {}, \
                     \"spike_rx_ts_predicted\": {}, \"retune_rx_ts\": {}, \
                     \"recovery_rx_ts\": {}, \"outage_rx_ts\": {}, \
                     \"level_before\": {:.2}, \"level_after\": {:.2}, \
                     \"latency_us\": {lat}}}{comma}",
                    e.from_hz,
                    e.to_hz,
                    m.spike_rx,
                    m.spike_rx_predicted,
                    m.retune_rx,
                    m.recovery_rx.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    m.outage_rx.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    m.level_before,
                    m.level_after,
                )?;
            }
            None => writeln!(
                f,
                "    {{\"from_hz\": {}, \"to_hz\": {}, \"spike_rx_ts\": null, \
                 \"latency_us\": null}}{comma}",
                e.from_hz, e.to_hz
            )?,
        }
    }
    writeln!(f, "  ]")?;
    writeln!(f, "}}")?;
    Ok(())
}
