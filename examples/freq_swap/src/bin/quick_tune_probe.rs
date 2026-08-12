// quick_tune_probe — can this bladeRF do quick tune, and how fast is it?
//
// Three ways to move the radio from one channel to another, timed back to back
// on the same hardware in the same run so they are directly comparable:
//
//   set_frequency      the normal path. libbladeRF works out what the RFIC
//                      needs and programs it — on the host, or on the FPGA's
//                      NIOS core, depending on the tuning mode.
//   schedule_retune    same, but expressed as a retune command.
//   quick tune         recall a *pre-computed* profile captured earlier by
//                      bladerf_get_quick_tune. No PLL arithmetic, no band
//                      selection logic — just reload registers the AD9361
//                      already knows.
//
// The wrapper's `QuickTune` type cannot be used here: `bladerf_quick_tune` is a
// C union with a bladeRF1 arm (freqsel/vcocap/nint/nfrac/flags/xb_gpio) and a
// bladeRF2 arm (nios_profile/rffe_profile/port/spdt), and the wrapper hardcodes
// the bladeRF1 one. On a bladeRF 2.0 micro that reads and writes the wrong
// fields — and passing a smaller struct would let libbladeRF write past it. So
// this probe drops to the raw bindgen type and picks the right arm by hand.
// That is exactly the fix the wrapper needs; this file is the proof it works.
//
// Run:
//   cd examples/freq_swap
//   BLADERF_INCLUDE_PATH=/usr/local/include RUSTFLAGS="-L/usr/local/lib64" \
//       cargo run --release --bin quick_tune_probe -- --stream

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ::bladerf::sys::*;
use ::bladerf::{
    BladeRF, BladeRfAny, Channel, ChannelLayoutRx, ComplexI16, RxChannel, StreamConfig, TuningMode,
};
use clap::{Parser, ValueEnum};

/// `BLADERF_RETUNE_NOW` — a C macro (`(bladerf_timestamp)0`), so bindgen does
/// not emit it. Retune immediately rather than at a future sample timestamp.
const RETUNE_NOW: u64 = 0;

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Tuning {
    Host,
    Fpga,
    Default,
}

#[derive(Parser, Debug)]
#[command(about = "Measure set_frequency vs schedule_retune vs quick tune on a bladeRF.")]
struct Args {
    /// Zigbee channel 15 centre, Hz.
    #[arg(long, default_value_t = 2_425_000_000)]
    freq_z: u64,
    /// 802.11ah centre, Hz.
    #[arg(long, default_value_t = 919_000_000)]
    freq_h: u64,
    /// Hardware sample rate (Hz).
    #[arg(long, default_value_t = 20e6)]
    sample_rate: f64,
    /// Run an RX stream during the measurement.
    #[arg(long)]
    stream: bool,
    /// Tuning mode to select first.
    #[arg(long, value_enum, default_value_t = Tuning::Fpga)]
    tuning_mode: Tuning,
    /// Hops to time per method (alternating Z/H, so each is cross-band).
    #[arg(long, default_value_t = 200)]
    hops: usize,
    /// Skip the timing loops; instead prove the quick tune physically moves the
    /// radio by comparing received power against set_frequency on both bands.
    #[arg(long)]
    verify_power: bool,
}

/// `bladerf_quick_tune` filled in for a bladeRF 2. Always the full union type,
/// so libbladeRF can write every byte it thinks it owns.
fn zeroed_quick_tune() -> bladerf_quick_tune {
    // SAFETY: the union is plain old data; all-zero is a valid bit pattern.
    unsafe { std::mem::zeroed() }
}

/// The bladeRF2 arm of the union, for printing.
fn brf2_arm(qt: &bladerf_quick_tune) -> (u16, u8, u8, u8) {
    // SAFETY: this board is a bladeRF 2, so arm 2 is the live one.
    unsafe {
        let a = qt.__bindgen_anon_1.__bindgen_anon_2;
        (a.nios_profile, a.rffe_profile, a.port, a.spdt)
    }
}

fn ptr_of(dev: &Arc<BladeRfAny>) -> *mut bladerf {
    dev.get_device_ptr()
}

/// Mean |IQ|^2 over a fresh capture, in dB relative to full scale.
fn band_power(
    rx: &::bladerf::RxSyncStream<Arc<BladeRfAny>, ComplexI16, BladeRfAny>,
    buf: &mut [ComplexI16],
) -> Result<f64> {
    // Two reads: the first may still hold pre-retune samples sitting in the
    // driver's buffers, the second is unambiguously post-retune.
    rx.read(buf, Duration::from_millis(500))?;
    rx.read(buf, Duration::from_millis(500))?;
    let sum: f64 = buf
        .iter()
        .map(|s| {
            let (i, q) = (s.re as f64, s.im as f64);
            i * i + q * q
        })
        .sum();
    Ok(10.0 * (sum / buf.len() as f64 / (2048.0 * 2048.0)).max(1e-30).log10())
}

/// Does a quick-tune recall actually move the radio?
///
/// `get_frequency` cannot answer this: in FPGA tuning mode it errors after a
/// recall, and in host mode it returns libbladeRF's cached idea of the
/// frequency, which `schedule_retune` does not update. So compare what the
/// antenna hears. If a recall lands on the same received power as an explicit
/// `set_frequency` to that band, and the two bands differ from each other, the
/// recall moved the radio.
fn verify_power(
    dev: &Arc<BladeRfAny>,
    ch: Channel,
    ch_raw: bladerf_channel,
    ptr: *mut bladerf,
    freq_z: u64,
    freq_h: u64,
) -> Result<()> {
    let rx = BladeRfAny::rx_streamer_arc::<ComplexI16>(
        dev.clone(),
        StreamConfig::default(),
        ChannelLayoutRx::SISO(RxChannel::Rx0),
    )?;
    rx.enable()?;
    let mut buf = vec![ComplexI16::new(0, 0); 65536];

    // Capture a profile per band, via set_frequency, measuring as we go.
    let mut rows = Vec::new();
    let mut profiles = Vec::new();
    for (label, hz) in [("Z", freq_z), ("H", freq_h)] {
        dev.set_frequency(ch, hz)?;
        thread::sleep(Duration::from_millis(100));
        let p = band_power(&rx, &mut buf)?;
        let mut qt = zeroed_quick_tune();
        // SAFETY: live device, full union type.
        let res = unsafe { bladerf_get_quick_tune(ptr, ch_raw, &mut qt) };
        if res != 0 {
            bail!("get_quick_tune({label}) failed: {res}");
        }
        rows.push((format!("set_frequency {label}"), hz, p));
        profiles.push((label, hz, qt));
    }

    // Now park on the *other* band first, so a recall that does nothing leaves
    // the radio measurably in the wrong place.
    for i in 0..2 {
        let other = &profiles[1 - i];
        dev.set_frequency(ch, other.1)?;
        thread::sleep(Duration::from_millis(100));

        let (label, hz, qt) = &mut profiles[i];
        // SAFETY: live device, full union type.
        let res = unsafe { bladerf_schedule_retune(ptr, ch_raw, RETUNE_NOW, *hz, qt) };
        if res != 0 {
            bail!("quick tune({label}) failed: {res}");
        }
        thread::sleep(Duration::from_millis(100));
        let p = band_power(&rx, &mut buf)?;
        rows.push((format!("quick tune    {label}"), *hz, p));
    }

    println!("\nreceived power after each retune (dBFS, mean |IQ|^2):");
    for (what, hz, p) in &rows {
        println!("  {what}  @ {:8.3} MHz   {p:8.2} dBFS", *hz as f64 / 1e6);
    }
    let (sz, sh, qz, qh) = (rows[0].2, rows[1].2, rows[2].2, rows[3].2);
    println!(
        "\n  band separation via set_frequency: {:.2} dB\n  \
         quick tune Z vs set_frequency Z:   {:.2} dB\n  \
         quick tune H vs set_frequency H:   {:.2} dB",
        (sz - sh).abs(),
        (qz - sz).abs(),
        (qh - sh).abs()
    );
    let verdict = if (sz - sh).abs() < 1.0 {
        "INCONCLUSIVE — the two bands sound the same, so this test cannot separate them"
    } else if (qz - sz).abs() < (sz - sh).abs() / 2.0 && (qh - sh).abs() < (sz - sh).abs() / 2.0 {
        "quick tune MOVES THE RADIO — each recall lands on its own band's power"
    } else {
        "quick tune did NOT move the radio — recalls do not track their band"
    };
    println!("\n  verdict: {verdict}");
    rx.disable()?;
    Ok(())
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p) as usize]
}

fn report(name: &str, mut v: Vec<f64>) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  {name:<26} n={:<4} min {:8.3}  med {:8.3}  p90 {:8.3}  max {:8.3}  ms",
        v.len(),
        v.first().copied().unwrap_or(f64::NAN),
        percentile(&v, 0.5),
        percentile(&v, 0.9),
        v.last().copied().unwrap_or(f64::NAN),
    );
}

fn main() -> Result<()> {
    let args = Args::parse();
    let dev = Arc::new(BladeRfAny::open_first().context("cannot open bladeRF")?);
    let ch = Channel::Rx0;
    let ch_raw = ch as bladerf_channel;

    dev.set_sample_rate(ch, args.sample_rate as u32)?;
    match args.tuning_mode {
        Tuning::Host => dev.set_tuning_mode(TuningMode::Host)?,
        Tuning::Fpga => dev.set_tuning_mode(TuningMode::FPGA)?,
        Tuning::Default => {}
    }
    let mode = dev.get_tuning_mode()?;
    println!(
        "tuning {mode:?}, {:.1} MHz <-> {:.1} MHz, {} hops each, stream={}",
        args.freq_z as f64 / 1e6,
        args.freq_h as f64 / 1e6,
        args.hops,
        args.stream
    );

    if args.verify_power {
        return verify_power(&dev, ch, ch_raw, ptr_of(&dev), args.freq_z, args.freq_h);
    }

    // Optional concurrent reader — a retune that stalls the stream is a very
    // different animal from one that merely returns slowly.
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let errs = Arc::new(AtomicU64::new(0));
    let reader = args.stream.then(|| {
        let (dev, stop, reads, errs) = (dev.clone(), stop.clone(), reads.clone(), errs.clone());
        thread::spawn(move || -> Result<()> {
            let rx = BladeRfAny::rx_streamer_arc::<ComplexI16>(
                dev,
                StreamConfig::default(),
                ChannelLayoutRx::SISO(RxChannel::Rx0),
            )?;
            rx.enable()?;
            let mut buf = vec![ComplexI16::new(0, 0); 8192];
            while !stop.load(Ordering::Relaxed) {
                match rx.read(&mut buf, Duration::from_millis(200)) {
                    Ok(()) => reads.fetch_add(1, Ordering::Relaxed),
                    Err(_) => errs.fetch_add(1, Ordering::Relaxed),
                };
            }
            rx.disable()?;
            Ok(())
        })
    });
    if args.stream {
        thread::sleep(Duration::from_millis(300));
    }

    // ── Capture one quick-tune profile per channel ────────────────────────
    // Precondition from the libbladeRF docs: the device must already be tuned
    // to the frequency whose profile is being captured.
    let ptr = dev.get_device_ptr();
    let mut profiles = Vec::new();
    for (label, hz) in [("Z", args.freq_z), ("H", args.freq_h)] {
        dev.set_frequency(ch, hz)?;
        thread::sleep(Duration::from_millis(50));
        let mut qt = zeroed_quick_tune();
        let t = Instant::now();
        // SAFETY: `ptr` is a live device, `qt` is the full union type so
        // libbladeRF cannot write out of bounds.
        let res = unsafe { bladerf_get_quick_tune(ptr, ch_raw, &mut qt) };
        if res != 0 {
            bail!("bladerf_get_quick_tune({label}) failed: {res}");
        }
        let (nios, rffe, port, spdt) = brf2_arm(&qt);
        println!(
            "  captured {label} @ {:.3} MHz in {:.3} ms  \
             nios_profile={nios} rffe_profile={rffe} port={port} spdt={spdt}",
            hz as f64 / 1e6,
            t.elapsed().as_secs_f64() * 1000.0
        );
        profiles.push((label, hz, qt));
    }

    // ── 1. set_frequency ──────────────────────────────────────────────────
    let mut t_setfreq = Vec::with_capacity(args.hops);
    for i in 0..args.hops {
        let (_, hz, _) = &profiles[i % 2];
        let t = Instant::now();
        dev.set_frequency(ch, *hz)?;
        t_setfreq.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // ── 2. schedule_retune, RETUNE_NOW, no profile ────────────────────────
    // Documented as unsupported on the bladeRF 2.0 micro (a NULL quick_tune is
    // rejected), so this is expected to fail — measured anyway to confirm it
    // is the profile that is required, not the call.
    let mut t_sched = Vec::with_capacity(args.hops);
    let mut sched_err = 0i32;
    for i in 0..args.hops.min(20) {
        let (_, hz, _) = &profiles[i % 2];
        let t = Instant::now();
        // SAFETY: live device; a null profile is what is being tested.
        let res = unsafe {
            bladerf_schedule_retune(ptr, ch_raw, RETUNE_NOW, *hz, std::ptr::null_mut())
        };
        if res != 0 {
            sched_err = res;
            break;
        }
        t_sched.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // ── 3. quick tune ─────────────────────────────────────────────────────
    let mut t_quick = Vec::with_capacity(args.hops);
    let mut quick_err = 0i32;
    for i in 0..args.hops {
        let (_, hz, qt) = &mut profiles[i % 2];
        let t = Instant::now();
        // SAFETY: live device; `qt` is the full union type captured above.
        let res = unsafe {
            bladerf_schedule_retune(ptr, ch_raw, RETUNE_NOW, *hz, qt)
        };
        if res != 0 {
            quick_err = res;
            break;
        }
        t_quick.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // ── Verify the quick tune actually moved the radio ────────────────────
    // 0.27 ms is about one USB round trip, which is also what a no-op would
    // cost — so read the frequency back after each recall rather than trusting
    // the return code.
    println!("\nreadback after quick tune:");
    for i in 0..2 {
        let (label, hz, qt) = &mut profiles[i];
        // SAFETY: live device, full union type.
        let res = unsafe { bladerf_schedule_retune(ptr, ch_raw, RETUNE_NOW, *hz, qt) };
        thread::sleep(Duration::from_millis(20));
        let got = dev.get_frequency(ch)?;
        println!(
            "  recall {label}: asked {:.3} MHz, device reports {:.3} MHz  {}  (rc={res})",
            *hz as f64 / 1e6,
            got as f64 / 1e6,
            if got.abs_diff(*hz) < 1000 { "MATCH" } else { "MISMATCH" },
        );
    }

    println!("\nresults ({mode:?} tuning):");
    report("set_frequency", t_setfreq);
    if sched_err != 0 {
        println!("  {:<26} FAILED after {} ok, code {sched_err}", "schedule_retune (null)", t_sched.len());
    } else {
        report("schedule_retune (null)", t_sched);
    }
    if quick_err != 0 {
        println!("  {:<26} FAILED after {} ok, code {quick_err}", "quick tune", t_quick.len());
    } else {
        report("quick tune", t_quick);
    }

    stop.store(true, Ordering::Relaxed);
    if let Some(h) = reader {
        let _ = h.join();
        println!(
            "\nstream: {} reads ok, {} failed",
            reads.load(Ordering::Relaxed),
            errs.load(Ordering::Relaxed)
        );
    }
    // Leave no scheduled retunes behind for the next process.
    let _ = dev.cancel_scheduled_retune(ch);
    Ok(())
}
