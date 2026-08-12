// zigbee_swap_quicktune — per-frame channel swap, retuned by quick tune.
//
// Same experiment as `zigbee_swap` (every decoded frame triggers a swap to the
// other channel), but the radio is driven by libbladeRF directly instead of
// SoapySDR, and every retune is a quick-tune profile recall.
//
// Why the driver had to change: SoapySDR claims the USB device, so a second
// libbladeRF handle cannot open alongside it ("No devices available"), and
// SoapySDR exposes no way to reach `bladerf_schedule_retune`. Quick tune is
// therefore only available to a program that owns the radio. So this binary
// drops the SoapySDR head flowgraph entirely:
//
//     BladeRfAny  ──reader thread──>  shared VecDeque<Complex32>
//                                            │  connect_radio()
//                                            v
//                                     swappable flowgraph
//
// `connect_radio` is the controller's hook for an externally fed C32 buffer,
// so no head TOML and no `seify_source_plugin` are involved. Retunes arrive
// through `set_fast_freq_setter`, which the controller already calls off the
// critical path — here it resolves the requested frequency to a pre-registered
// profile and recalls it.
//
// Measured on a bladeRF 2.0 micro, 919 MHz <-> 2425 MHz:
//     set_frequency, host tuning     120.0 ms
//     set_frequency, FPGA tuning      26.2 ms
//     quick tune                       0.273 ms
//
// Channel plans registered at startup:
//     Z11..Z26   802.15.4, 2405..2480 MHz, 5 MHz spacing        (16)
//     H2..H50    802.11ah US 2 MHz, 903..927 MHz, 2 MHz spacing (13)
//
// Output: zigbee_swap_quicktune.csv, same schema as zigbee_swap.csv.
// Run from this directory:
//   cd examples/real_device_swap
//   BLADERF_INCLUDE_PATH=/usr/local/include RUSTFLAGS="-L/usr/local/lib64" \
//       cargo build --release --bin zigbee_swap_quicktune
//   LD_LIBRARY_PATH=/usr/local/lib64 ../../target/release/zigbee_swap_quicktune

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bladerf::sys::{bladerf_channel, bladerf_get_quick_tune, bladerf_quick_tune,
                   bladerf_schedule_retune};
use bladerf::{
    BladeRF, BladeRfAny, Channel, ChannelLayoutRx, ComplexI16, RxChannel, StreamConfig, TuningMode,
};
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::futuredsp::firdes;
use futuresdr::futuredsp::prelude::*;
use futuresdr::futuredsp::DecimatingFirFilter;
use futuresdr::futures::{select, FutureExt, StreamExt};
use futuresdr::num_complex::Complex32;
use futuresdr::runtime::Pmt;
use plugin_host::{default_plugin_dir, FlowgraphController};

/// `BLADERF_RETUNE_NOW` — a C macro, so bindgen does not emit it. Apply the
/// retune as soon as it arrives rather than at a future sample timestamp.
const RETUNE_NOW: u64 = 0;

/// SC16_Q11 full scale. The AD9361 delivers 12-bit samples left-aligned in
/// 16 bits, so +-2048 maps to +-1.0.
const SC16_Q11_SCALE: f32 = 1.0 / 2048.0;

const FLOW_ZIGBEE_A: &str = "flows/zigbee_rxA.toml";
const FLOW_ZIGBEE_B: &str = "flows/zigbee_rxB.toml";

/// Channels registered for quick tune at startup, in MHz.
///
/// A registry, not a schedule: a recall is a lookup by frequency, so any of
/// these is reachable in ~0.27 ms whenever a flow asks for it, in any order.
/// Adding a flow on one of these frequencies needs no change here.
///
/// Kept to four on purpose. The AD9361 holds 8 RFFE fastlock slots, so
/// registering all 29 standard channels wraps `rffe_profile` three times and
/// several channels end up sharing a slot — 2450 and 921 among them. Four
/// channels each get a slot of their own.
const QUICKTUNE_CHANNELS_MHZ: &[f64] = &[919.0, 921.0, 2425.0, 2450.0];
const TAIL_FLOW: &str = "flows/null_tail.toml";
const CSV_PATH: &str = "zigbee_swap_quicktune.csv";

/// If no tap frame arrives within this time after the last swap, swap anyway.
const RX_TIMEOUT: Duration = Duration::from_millis(80000);

/// Cap on the shared deque, in samples. Matches the controller's own C32
/// bridge capacity so behaviour does not change with the feed path.
const BUF_CAP: usize = 4_194_304;

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

#[derive(Parser, Debug)]
#[command(about = "Per-frame Zigbee channel swap, retuned by bladeRF quick tune.")]
struct Args {
    /// Print the channel plans and exit, without touching the radio.
    #[arg(long)]
    list_channels: bool,
    /// Register every channel, report the profile each was assigned, and exit.
    /// Use this to check how many profiles the RFIC will actually hold.
    #[arg(long)]
    register_only: bool,
    /// Recall every registered profile after registering and report received
    /// power per channel, so a profile that was silently evicted shows up.
    #[arg(long)]
    verify: bool,
    /// Hardware sample rate (Hz).
    ///
    /// Tracks `sdr_head.toml`, so the SoapySDR path and this one run the same
    /// configuration and their results are comparable. At 4 MSps with
    /// `--decim 1` the device feeds the PHY directly and no decimation filter
    /// is involved; raise both together (e.g. 20e6 / 5) to put the head's
    /// filter back in front.
    #[arg(long, default_value_t = 20e6)]
    sample_rate: f64,
    /// Software decimation applied to the hardware stream, matching the head's
    /// `fir_resampler_plugin` 1:N. `--sample-rate / --decim` must equal the
    /// rate the flows demand.
    #[arg(long, default_value_t = 5)]
    decim: usize,
    /// Samples per USB transfer buffer (multiple of 1024).
    ///
    /// This sets how much of the antenna's past the driver holds before the
    /// PHY sees it: `stream_buffer * stream_buffers / sample_rate`. The
    /// default 4096 x 16 at 20 MSps is ~3.3 ms. That latency matters here
    /// because it is the same order as the emitter's 10 ms dwell — samples
    /// captured before a swap keep arriving after it, on the wrong channel.
    #[arg(long, default_value_t = 4096)]
    stream_buffer: usize,
    /// Number of transfer buffers; must exceed `--stream-transfers`.
    #[arg(long, default_value_t = 16)]
    stream_buffers: u32,
    /// In-flight USB transfers.
    #[arg(long, default_value_t = 8)]
    stream_transfers: u32,
    /// Samples to discard after each swap, in microseconds of air time.
    ///
    /// Clearing the deque is not enough: the driver still holds samples
    /// captured *before* the swap, and feeding that stale tail to the freshly
    /// built flowgraph makes it decode a fragment of the frame that triggered
    /// the swap — a duplicate ~0.4 ms behind the real one, with a payload the
    /// stamp parser cannot read.
    ///
    /// Defaults to 0 — **measured to have no effect**. Flushing 0, 300, 600
    /// and 1200 us all leave the duplicate rate at 59-60%, which rules the
    /// driver backlog out as their source. Kept as a knob because it is the
    /// obvious thing to try, and now it is answered rather than assumed.
    #[arg(long, default_value_t = 0)]
    flush_us: u64,
    /// Hand gain control to the RFIC instead of fixing it at `--gain-db`.
    /// Convenient across bands of very different power, but it re-converges
    /// after every swap, so it is off by default.
    #[arg(long)]
    agc: bool,
    /// RX gain (dB) applied at startup, tracking `sdr_head.toml`. Note that
    /// the flows' `[radio] gain_db` overrides this on the first swap.
    #[arg(long, default_value_t = 10)]
    gain_db: i32,
    /// Fall back to `set_frequency` instead of quick tune — the control for
    /// measuring what quick tune is worth, on otherwise identical machinery.
    #[arg(long)]
    no_quick_tune: bool,
    /// Register every channel of both plans (29) instead of the four in
    /// [`QUICKTUNE_CHANNELS_MHZ`].
    ///
    /// Off by default, and worth understanding before turning on: the AD9361
    /// holds 8 RFFE fastlock slots, so registering 29 channels wraps
    /// `rffe_profile` three times and several channels end up sharing a slot.
    /// The default four each keep a slot of their own.
    #[arg(long)]
    register_all: bool,
}

// ── Channel plans ────────────────────────────────────────────────────────

/// 802.15.4 2.4 GHz: channels 11..26, 5 MHz spacing, 2405..2480 MHz.
fn zigbee_channels() -> Vec<(String, u64)> {
    (11..=26)
        .map(|ch| (format!("Z{ch}"), 2_405_000_000 + 5_000_000 * (ch - 11) as u64))
        .collect()
}

/// 802.11ah US, 2 MHz channels.
///
/// S1G numbering puts the centre at `902.0 MHz + 0.5 * channel`, and the 2 MHz
/// plan uses channel numbers 2, 6, 10, ... 50 — so centres run 903..927 MHz in
/// 2 MHz steps, 13 channels. (919 MHz, the one these flows use, is channel 34.)
fn halow_us_2mhz_channels() -> Vec<(String, u64)> {
    (0..13)
        .map(|i| {
            let ch = 2 + 4 * i;
            (format!("H{ch}"), 902_000_000 + 500_000 * ch as u64)
        })
        .collect()
}

fn all_channels() -> Vec<(String, u64)> {
    let mut v = zigbee_channels();
    v.extend(halow_us_2mhz_channels());
    v
}

/// The channels named in [`QUICKTUNE_CHANNELS_MHZ`], labelled from the standard
/// plans where they are standard channels.
fn selected_channels() -> Vec<(String, u64)> {
    let named: HashMap<u64, String> = all_channels().into_iter().map(|(l, h)| (h, l)).collect();
    QUICKTUNE_CHANNELS_MHZ
        .iter()
        .map(|mhz| {
            let hz = (mhz * 1e6).round() as u64;
            let label = named
                .get(&hz)
                .cloned()
                .unwrap_or_else(|| format!("{mhz:.3}MHz"));
            (label, hz)
        })
        .collect()
}

/// The `[radio] frequency_hz` a flow declares.
fn flow_frequency_hz(path: &str) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
    let doc: toml::Value = text.parse().map_err(|e| format!("invalid TOML '{path}': {e}"))?;
    let hz = doc
        .get("radio")
        .and_then(|r| r.get("frequency_hz"))
        .and_then(|f| f.as_float().or_else(|| f.as_integer().map(|i| i as f64)))
        .ok_or_else(|| format!("'{path}' has no [radio] frequency_hz"))?;
    Ok(hz.round() as u64)
}

// ── Quick-tune registry ──────────────────────────────────────────────────

/// One registered channel: its label and the profile the RFIC handed back.
struct Registered {
    label: String,
    hz: u64,
    qt: bladerf_quick_tune,
    nios_profile: u16,
    rffe_profile: u8,
}

/// Every channel's quick-tune profile, looked up by frequency.
///
/// The lookup is by nearest frequency within a kHz rather than exact equality:
/// the flows express frequencies as TOML floats (`2.425e9`), and the retune
/// path hands them over as `f64`.
struct Registry {
    dev: Arc<BladeRfAny>,
    channel: Channel,
    by_hz: Mutex<HashMap<u64, bladerf_quick_tune>>,
    /// Recalls that found no profile and fell back to `set_frequency`.
    misses: AtomicU64,
}

// SAFETY: `bladerf_quick_tune` is a POD union of integers, and `BladeRfAny` is
// already Send + Sync. The only shared mutable state is behind the Mutex.
unsafe impl Send for Registry {}
unsafe impl Sync for Registry {}

impl Registry {
    /// Tune to each channel in turn and snapshot its profile.
    ///
    /// `bladerf_get_quick_tune` requires the device to already be on the
    /// frequency being captured, so this is inherently one slow retune per
    /// channel — ~10 ms each in host tuning mode, paid once at startup.
    fn register(
        dev: Arc<BladeRfAny>,
        channel: Channel,
        channels: &[(String, u64)],
    ) -> Result<(Self, Vec<Registered>), Box<dyn std::error::Error + Send + Sync>> {
        let ptr = dev.get_device_ptr();
        let ch_raw = channel as bladerf_channel;
        let mut map = HashMap::new();
        let mut out = Vec::new();

        for (label, hz) in channels {
            dev.set_frequency(channel, *hz)?;
            // The RFIC needs to be settled before its state is worth capturing.
            std::thread::sleep(Duration::from_millis(5));

            // SAFETY: live device; `qt` is the full union type, so libbladeRF
            // cannot write past it whichever arm it fills in.
            let mut qt: bladerf_quick_tune = unsafe { std::mem::zeroed() };
            let res = unsafe { bladerf_get_quick_tune(ptr, ch_raw, &mut qt) };
            if res != 0 {
                return Err(format!("get_quick_tune({label} @ {hz} Hz) failed: {res}").into());
            }
            // SAFETY: this is a bladeRF 2, so the second union arm is live.
            let (nios_profile, rffe_profile) = unsafe {
                let a = qt.__bindgen_anon_1.__bindgen_anon_2;
                (a.nios_profile, a.rffe_profile)
            };
            map.insert(*hz, qt);
            out.push(Registered {
                label: label.clone(),
                hz: *hz,
                qt,
                nios_profile,
                rffe_profile,
            });
        }

        Ok((
            Self {
                dev,
                channel,
                by_hz: Mutex::new(map),
                misses: AtomicU64::new(0),
            },
            out,
        ))
    }

    /// Recall the profile for `hz`. Falls back to `set_frequency` if that
    /// channel was never registered, so an unregistered flow still tunes —
    /// slowly, and counted, rather than silently sitting on the wrong band.
    fn retune(&self, hz: f64) {
        let target = hz.round() as u64;
        let found = {
            let map = self.by_hz.lock().unwrap();
            map.get(&target).copied().or_else(|| {
                map.iter()
                    .find(|(k, _)| k.abs_diff(target) < 1_000)
                    .map(|(_, v)| *v)
            })
        };

        match found {
            Some(mut qt) => {
                // SAFETY: live device; `qt` is a full union captured from it.
                let res = unsafe {
                    bladerf_schedule_retune(
                        self.dev.get_device_ptr(),
                        self.channel as bladerf_channel,
                        RETUNE_NOW,
                        target,
                        &mut qt,
                    )
                };
                if res != 0 {
                    eprintln!("[quick tune] recall {target} Hz failed: {res}");
                }
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[quick tune] {target} Hz not registered — falling back to set_frequency"
                );
                if let Err(e) = self.dev.set_frequency(self.channel, target) {
                    eprintln!("[quick tune] fallback set_frequency failed: {e}");
                }
            }
        }
    }
}

// ── Frame parsing (identical to zigbee_swap) ─────────────────────────────

/// Strip the RFTAP encapsulation, returning the 802.15.4 MAC frame.
///
/// Header: magic `RFta`, a u16 length in 32-bit words, a u16 present-flags
/// field, then the optional fields the flags select; the encapsulated frame
/// follows. Parsing the stamp out of the MAC frame rather than out of the
/// whole blob means the search can never match inside RFTAP's own header.
///
/// Returns the blob unchanged if it is not RFTAP-wrapped, so a tap that
/// delivers a bare frame still works.
fn strip_rftap(blob: &[u8]) -> &[u8] {
    if blob.len() < 8 || &blob[0..4] != b"RFta" {
        return blob;
    }
    let header_len = u16::from_le_bytes([blob[4], blob[5]]) as usize * 4;
    if header_len < 8 || header_len > blob.len() {
        return blob;
    }
    &blob[header_len..]
}

/// The 802.15.4 MAC sequence number (DSN), byte 2 of the MHR.
///
/// Present on every data frame whether or not the payload carries a stamp, so
/// it identifies a frame even when the stamp cannot be located — and a row
/// repeating the previous row's DSN is the same transmission decoded twice,
/// not a new one.
fn parse_dsn(blob: &[u8]) -> Option<u8> {
    let frame = strip_rftap(blob);
    (frame.len() > 2).then(|| frame[2])
}

/// Parse the 19-byte stamp the transmitter puts in the payload:
///
/// ```text
///   [0..4)   frame#   u32   frame number within this IFS step
///   [4..6)   idx      u16   step index
///   [6]      tag      u8    channel number (0x0F = ch15, 0x14 = ch20)
///   [7..11)  ifs_us   u32   IFS of this step
///   [11..19) ts       u64   microseconds since boot
/// ```
///
/// all little-endian, located by the fixed EUI-64 source address that
/// immediately precedes it — identical to `zigbee_swap.rs`.
///
/// An earlier version fell back to scanning for a stamp-shaped run of bytes
/// when the anchor was absent. That recovered nothing (the frames without the
/// anchor genuinely carry no stamp) and could match arbitrary payload bytes,
/// producing rows of nonsense. Frames whose stamp cannot be located are now
/// reported as unstamped rather than guessed at.
fn parse_payload(blob: &[u8]) -> Option<(u32, u16, u8, u32, u64)> {
    let frame = strip_rftap(blob);
    let needed = ZIGBEE_EUI64_ANCHOR.len() + 19;
    if frame.len() < needed {
        return None;
    }
    for i in 0..=(frame.len() - needed) {
        if frame[i..i + ZIGBEE_EUI64_ANCHOR.len()] != ZIGBEE_EUI64_ANCHOR {
            continue;
        }
        let p = i + ZIGBEE_EUI64_ANCHOR.len();
        return Some((
            u32::from_le_bytes([frame[p], frame[p + 1], frame[p + 2], frame[p + 3]]),
            u16::from_le_bytes([frame[p + 4], frame[p + 5]]),
            frame[p + 6],
            u32::from_le_bytes([frame[p + 7], frame[p + 8], frame[p + 9], frame[p + 10]]),
            u64::from_le_bytes([
                frame[p + 11], frame[p + 12], frame[p + 13], frame[p + 14],
                frame[p + 15], frame[p + 16], frame[p + 17], frame[p + 18],
            ]),
        ));
    }
    None
}

fn other(curr: &str) -> &'static str {
    if curr == FLOW_ZIGBEE_A { FLOW_ZIGBEE_B } else { FLOW_ZIGBEE_A }
}

fn phy_name(toml: &str) -> &'static str {
    if toml == FLOW_ZIGBEE_A { "A" } else { "B" }
}

// ── Main ─────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    let channels = if args.register_all {
        all_channels()
    } else {
        selected_channels()
    };

    if args.list_channels {
        println!(
            "{} channels to register ({}):\n",
            channels.len(),
            if args.register_all { "--register-all" } else { "QUICKTUNE_CHANNELS_MHZ" }
        );
        for (label, hz) in &channels {
            println!("  {label:>4}  {:9.3} MHz", *hz as f64 / 1e6);
        }
        return Ok(());
    }

    futuresdr::runtime::init();
    println!("=== zigbee_swap_quicktune — per-frame swap, quick-tune retune ===");

    // ── Open the radio and register every channel ────────────────────────
    let dev = Arc::new(BladeRfAny::open_first()?);
    println!("bladeRF {}", dev.get_serial().unwrap_or_default());
    let ch = Channel::Rx0;
    // Tuning mode FIRST. Switching RFIC control between host and FPGA
    // re-initialises the AD9361 and discards the sample rate, bandwidth and
    // gain set before it — the device comes back at its 30.72 MSps default.
    // Setting it up front means everything below sticks.
    //
    // The mode only affects `set_frequency`, not profile recalls; it is set so
    // the fallback path and the `--no-quick-tune` control are as fast as they
    // can be, keeping the comparison about quick tune alone.
    dev.set_tuning_mode(TuningMode::FPGA)?;
    let actual_rate = dev.set_sample_rate(ch, args.sample_rate as u32)?;
    let actual_bw = dev.set_bandwidth(ch, args.sample_rate as u32)?;
    if args.agc {
        dev.set_gain_mode(ch, bladerf::GainMode::SlowAttackAgc)?;
    } else {
        dev.set_gain(ch, args.gain_db)?;
    }
    // Anything that reconfigures the RFIC silently reverts these, and the
    // symptom is a receiver that streams happily and decodes nothing. Check
    // rather than trust.
    if (actual_rate as f64 - args.sample_rate).abs() > 1.0 {
        return Err(format!(
            "asked for {:.3} MSps, device gave {:.3} MSps",
            args.sample_rate / 1e6,
            actual_rate as f64 / 1e6
        )
        .into());
    }
    println!(
        "  {:.3} MSps hw / {:.3} MSps to the PHY (decim {}), bw {:.3} MHz, {}, tuning {:?}\n  \
         driver buffering {:.2} ms ({} x {} samples)",
        actual_rate as f64 / 1e6,
        actual_rate as f64 / args.decim.max(1) as f64 / 1e6,
        args.decim,
        actual_bw as f64 / 1e6,
        if args.agc { "AGC".to_string() } else { format!("gain {} dB", args.gain_db) },
        dev.get_tuning_mode()?,
        1e3 * (args.stream_buffer as f64 * args.stream_buffers as f64) / actual_rate as f64,
        args.stream_buffers,
        args.stream_buffer,
    );

    let t_reg = Instant::now();
    let (registry, registered) = Registry::register(dev.clone(), ch, &channels)?;
    let registry = Arc::new(registry);
    println!(
        "\nregistered {} channels in {:.1} ms:",
        registered.len(),
        t_reg.elapsed().as_secs_f64() * 1000.0
    );
    for r in &registered {
        println!(
            "  {:>4}  {:9.3} MHz   nios_profile={:<4} rffe_profile={}",
            r.label,
            r.hz as f64 / 1e6,
            r.nios_profile,
            r.rffe_profile
        );
    }

    // The AD9361 holds a bounded number of RFFE fastlock profiles. If the
    // assignments repeat, later registrations evicted earlier ones and those
    // channels can no longer be recalled — say so rather than let it surface
    // later as an unexplained dead channel.
    let mut seen: HashMap<u8, &str> = HashMap::new();
    let mut collisions = Vec::new();
    for r in &registered {
        if let Some(prev) = seen.insert(r.rffe_profile, &r.label) {
            collisions.push((prev.to_string(), r.label.clone(), r.rffe_profile));
        }
    }
    if collisions.is_empty() {
        println!("\nall {} profiles distinct — every channel is recallable", registered.len());
    } else {
        println!(
            "\nWARNING: {} rffe_profile collisions — the RFIC holds only so many \
             fastlock slots, so the earlier channel of each pair may have been evicted:",
            collisions.len()
        );
        for (a, b, p) in collisions.iter().take(8) {
            println!("    {a} and {b} both got rffe_profile={p}");
        }
        println!("  --verify measures which ones still work.");
    }

    if args.register_only && !args.verify {
        return Ok(());
    }

    // ── Stream ───────────────────────────────────────────────────────────
    let buf: plugin_host::RadioOutputBuf = Arc::new(Mutex::new(Default::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let overruns = Arc::new(AtomicU64::new(0));
    // Sample accounting, so the heartbeat can tell "no signal" from "no
    // samples" — they look identical from the decoder's side.
    let pushed = Arc::new(AtomicU64::new(0));
    /// Gates the reader the way a `BridgeSink`'s `connected` flag gates the
    /// head flowgraph. Without it the reader keeps writing into the deque
    /// while `swap()` is clearing it, so the incoming flowgraph inherits a
    /// spliced stream — which the PHY reads as preambles that were never
    /// transmitted. Measured: 38% of "decoded" frames were fabricated this
    /// way, against 0% on the SoapySDR head path.
    let feeding = Arc::new(AtomicBool::new(true));
    /// Hardware samples still to be discarded after the most recent swap.
    let discard = Arc::new(AtomicU64::new(0));
    let read_errs = Arc::new(AtomicU64::new(0));
    /// Peak |IQ| seen since the last heartbeat, scaled by 1e6 to fit an atomic.
    let peak_ppm = Arc::new(AtomicU64::new(0));

    let reader = {
        let (dev, buf, stop, overruns) =
            (dev.clone(), buf.clone(), stop.clone(), overruns.clone());
        let (pushed, read_errs, peak_ppm) =
            (pushed.clone(), read_errs.clone(), peak_ppm.clone());
        let stream_buffer = args.stream_buffer;
        let feeding = feeding.clone();
        let discard = discard.clone();
        let decim = args.decim.max(1);
        let stream_cfg = StreamConfig::new(
            args.stream_buffers,
            args.stream_buffer,
            args.stream_transfers,
            Duration::from_millis(3500),
        )?;
        std::thread::spawn(move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let rx = BladeRfAny::rx_streamer_arc::<ComplexI16>(
                dev,
                stream_cfg,
                ChannelLayoutRx::SISO(RxChannel::Rx0),
            )?;
            rx.enable()?;
            let mut raw = vec![ComplexI16::new(0, 0); stream_buffer];
            let mut conv: Vec<Complex32> = Vec::with_capacity(raw.len());

            // Same filter the head builds: FirBuilder::resampling(1, decim)
            // designs kaiser::multirate(1, decim, 12, 0.0001), so the PHY sees
            // the band it was tuned against rather than a raw downsample.
            let taps: Vec<f32> = firdes::kaiser::multirate(1, decim, 12, 0.0001);
            let n_hist = taps.len().saturating_sub(1);
            let fir = DecimatingFirFilter::<Complex32, Complex32, Vec<f32>>::new(decim, taps);
            // Carries the filter's tail between reads; without it every buffer
            // boundary would be a discontinuity.
            let mut hist: Vec<Complex32> = vec![Complex32::new(0.0, 0.0); n_hist];
            let mut scratch: Vec<Complex32> = Vec::with_capacity(raw.len() + n_hist);
            let mut out: Vec<Complex32> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if rx.read(&mut raw, Duration::from_millis(200)).is_err() {
                    read_errs.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                // Post-swap flush: drop the driver's pre-swap backlog. The
                // filter history goes with it — what follows is a different
                // channel, so carrying state across would smear the boundary.
                let owed = discard.load(Ordering::Relaxed);
                if owed > 0 {
                    let n = (raw.len() as u64).min(owed);
                    discard.fetch_sub(n, Ordering::Relaxed);
                    hist.iter_mut().for_each(|h| *h = Complex32::new(0.0, 0.0));
                    continue;
                }
                conv.clear();
                let mut peak = 0.0f32;
                conv.extend(raw.iter().map(|s| {
                    let c = Complex32::new(
                        s.re as f32 * SC16_Q11_SCALE,
                        s.im as f32 * SC16_Q11_SCALE,
                    );
                    let m = c.re.abs().max(c.im.abs());
                    if m > peak {
                        peak = m;
                    }
                    c
                }));
                peak_ppm.fetch_max((peak * 1e6) as u64, Ordering::Relaxed);

                let produced: &[Complex32] = if decim == 1 {
                    &conv
                } else {
                    scratch.clear();
                    scratch.extend_from_slice(&hist);
                    scratch.extend_from_slice(&conv);
                    let n_out = scratch.len().saturating_sub(n_hist) / decim;
                    out.resize(n_out, Complex32::new(0.0, 0.0));
                    let (consumed, produced_n, _) = fir.filter(&scratch, &mut out);
                    let _ = consumed;
                    hist.clear();
                    hist.extend_from_slice(&scratch[scratch.len() - n_hist..]);
                    &out[..produced_n]
                };
                // Always drain the device — stopping reads would overflow the
                // driver — but discard rather than write while gated off.
                if !feeding.load(Ordering::Relaxed) {
                    continue;
                }
                pushed.fetch_add(produced.len() as u64, Ordering::Relaxed);
                let conv = produced;
                let mut q = buf.lock().unwrap();
                // Drop oldest to make room — the PHY reading this is the only
                // thing that matters, and a stalled consumer must not turn
                // into unbounded memory growth.
                let overflow = (q.len() + conv.len()).saturating_sub(BUF_CAP);
                if overflow > 0 {
                    q.drain(..overflow);
                    overruns.fetch_add(overflow as u64, Ordering::Relaxed);
                }
                q.extend(conv.iter().copied());
            }
            rx.disable()?;
            Ok(())
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    println!(
        "  stream live: device reports {:.3} MSps",
        dev.get_sample_rate(ch)? as f64 / 1e6
    );

    if args.verify {
        verify_recalls(&registry, &registered, &buf)?;
        stop.store(true, Ordering::Relaxed);
        let _ = reader.join();
        return Ok(());
    }

    // Registration leaves the radio on whichever channel was registered last,
    // so park it on the channel the initial flow expects. Without this the
    // receiver starts deaf and cannot recover: a swap is what retunes, and a
    // swap only happens once a frame arrives — which it never does.
    let start_hz = flow_frequency_hz(FLOW_ZIGBEE_A)?;
    registry.retune(start_hz as f64);
    println!(
        "\nInitial PHY: {} @ {:.3} MHz; alternating with {}",
        phy_name(FLOW_ZIGBEE_A),
        start_hz as f64 / 1e6,
        phy_name(FLOW_ZIGBEE_B),
    );
    println!("CSV → {CSV_PATH}\n");

    // ── Flowgraphs ───────────────────────────────────────────────────────
    // No head TOML: the radio is ours, and `connect_radio` feeds the
    // swappable's input port straight from the deque the reader fills.
    let tail_idx = 0usize;
    let swap_idx = 1usize;
    let _ = tail_idx;
    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_permanent(TAIL_FLOW)
        .add_swappable(FLOW_ZIGBEE_A)
        .connect_radio(buf.clone(), swap_idx, "in")
        .tap_channel(256);

    let quick = !args.no_quick_tune;
    let feeding_ctl = feeding.clone();
    let discard_ctl = discard.clone();
    let flush_samples = (args.flush_us as f64 * args.sample_rate / 1e6) as u64;
    let buf_ctl = buf.clone();
    let reg_for_setter = registry.clone();
    let dev_for_gain = dev.clone();

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        for &(idx, ref path, perm) in &entries {
            if perm {
                println!("Starting permanent fg/{idx}/ from '{path}'");
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                println!("Starting swappable fg/{idx}/ from '{path}'");
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        // The controller calls this off the critical path for every `[radio]`
        // frequency demand. Quick tune makes it ~0.27 ms.
        if quick {
            ctrl.set_fast_freq_setter(move |hz| reg_for_setter.retune(hz));
        } else {
            let dev = dev_for_gain.clone();
            ctrl.set_fast_freq_setter(move |hz| {
                if let Err(e) = dev.set_frequency(Channel::Rx0, hz.round() as u64) {
                    eprintln!("[set_frequency] {hz} failed: {e}");
                }
            });
        }
        {
            let dev = dev_for_gain.clone();
            ctrl.set_fast_gain_setter(move |g| {
                if let Err(e) = dev.set_gain(Channel::Rx0, g.round() as i32) {
                    eprintln!("[set_gain] {g} failed: {e}");
                }
            });
        }

        let swap_target = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("at least one swappable flowgraph");

        let mut csv = File::create(CSV_PATH)?;
        writeln!(
            csv,
            "rx_idx,phy_active,frame_event,dsn,step,run,tag,wait_us,ts_us,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        let mut current = FLOW_ZIGBEE_A;
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms: f64 = 0.0;
        let mut swap_count: u64 = 0;
        let t0 = Instant::now();

        // Silence is otherwise ambiguous — no signal, no samples and a dead
        // reader all look identical. This separates them.
        {
            let (buf, overruns, pushed, read_errs, peak_ppm) = (
                buf.clone(), overruns.clone(), pushed.clone(),
                read_errs.clone(), peak_ppm.clone(),
            );
            let dev_hb = dev.clone();
            std::thread::spawn(move || {
                let (mut last_ov, mut last_push) = (0u64, 0u64);
                loop {
                    std::thread::sleep(Duration::from_secs(2));
                    let depth = buf.lock().unwrap().len();
                    let ov = overruns.load(Ordering::Relaxed);
                    let np = pushed.load(Ordering::Relaxed);
                    let peak = peak_ppm.swap(0, Ordering::Relaxed) as f64 / 1e6;
                    let dev_rate = dev_hb.get_sample_rate(Channel::Rx0).unwrap_or(0) as f64 / 1e6;
                    println!(
                        "  [radio] {:.3} MSps in (dev {dev_rate:.3}), deque {depth}, \
                         peak {:.4} ({:.1} dBFS), {} dropped (+{}), {} read errors",
                        (np - last_push) as f64 / 2.0 / 1e6,
                        peak,
                        20.0 * peak.max(1e-9).log10(),
                        ov,
                        ov - last_ov,
                        read_errs.load(Ordering::Relaxed),
                    );
                    last_ov = ov;
                    last_push = np;
                }
            });
        }

        println!(
            "\nReceiver running ({}). Every decoded frame triggers a channel swap.",
            if quick { "quick tune" } else { "set_frequency" }
        );
        println!("Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            let mut deadline = FutureExt::fuse(Timer::after(RX_TIMEOUT));
            let frame_event;
            let mut payload_row: Option<(i64, i64, i64, i64, i128)> = None;
            let mut dsn: i64 = -1;

            select! {
                _ = deadline => {
                    frame_event = "timeout";
                    println!(
                        "[t={:.1}ms] [timeout] no frame on {} — swapping anyway",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        phy_name(current),
                    );
                }
                tap = tap_rx.next() => {
                    let Some((_name, pmt)) = tap else { break };
                    frame_event = "rx";
                    if let Pmt::Blob(blob) = &pmt {
                        if let Some(d) = parse_dsn(blob) {
                            dsn = d as i64;
                        }
                        if let Some((step, run, tag, wait_us, ts)) = parse_payload(blob) {
                            payload_row = Some((
                                step as i64, run as i64, tag as i64, wait_us as i64, ts as i128,
                            ));
                        }
                    }
                }
            }

            let rx_t_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let next = other(current);
            let t_swap = Instant::now();
            // Disconnect across the swap, reconnect once the new flowgraph is
            // running — the same make-before-break the bridge sink performs.
            feeding_ctl.store(false, Ordering::Relaxed);
            let swap_res = ctrl.swap(swap_target, next, &rt_handle).await;
            buf_ctl.lock().unwrap().clear();
            discard_ctl.store(flush_samples, Ordering::Relaxed);
            feeding_ctl.store(true, Ordering::Relaxed);
            if let Err(e) = swap_res {
                eprintln!("[policy] swap failed: {e}");
            } else {
                current = next;
            }
            let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
            swap_total_ms += swap_ms;
            swap_count += 1;

            let (step, run, tag, wait_us, ts) =
                payload_row.unwrap_or((-1, -1, -1, -1, -1));
            writeln!(
                csv,
                "{rx_idx},{},{frame_event},{dsn},{step},{run},{tag},{wait_us},{ts},\
                 {rx_t_ms:.3},{swap_ms:.3}",
                phy_name(current),
            )?;
            csv.flush().ok();
            rx_idx += 1;

            if rx_idx % 100 == 0 {
                println!(
                    "  {rx_idx} frames, mean swap {:.3} ms, {} quick-tune misses, {} overruns",
                    swap_total_ms / swap_count as f64,
                    registry.misses.load(Ordering::Relaxed),
                    overruns.load(Ordering::Relaxed),
                );
            }
        }

        stop.store(true, Ordering::Relaxed);
        Ok(())
    })
}

/// Recall every registered profile and measure what the antenna hears.
///
/// A profile that was evicted from the RFIC's fastlock slots does not
/// necessarily fail loudly — it can succeed and leave the radio where it was.
/// Comparing power after a recall against power after an explicit
/// `set_frequency` to the same channel catches that.
fn verify_recalls(
    registry: &Registry,
    registered: &[Registered],
    buf: &plugin_host::RadioOutputBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// Mean |IQ|^2 in dBFS over freshly arrived samples.
    fn power(buf: &plugin_host::RadioOutputBuf) -> f64 {
        buf.lock().unwrap().clear();
        std::thread::sleep(Duration::from_millis(60));
        let q = buf.lock().unwrap();
        if q.is_empty() {
            return f64::NAN;
        }
        let sum: f64 = q.iter().map(|s| (s.re * s.re + s.im * s.im) as f64).sum();
        10.0 * (sum / q.len() as f64).max(1e-30).log10()
    }

    println!("\nverifying every profile (set_frequency vs quick-tune recall):");
    println!(
        "  {:>4}  {:>10}  {:>9}  {:>9}  {:>9}   {}",
        "ch", "MHz", "park dBFS", "set dBFS", "qt dBFS", "verdict"
    );
    let (mut ok_n, mut bad, mut inconclusive) = (0, 0, 0);
    for (i, r) in registered.iter().enumerate() {
        // Park on the *other band*, which is the only pairing with enough
        // power contrast on this setup to tell channels apart at all.
        let elsewhere = if r.label.starts_with('Z') {
            registered.iter().rfind(|x| x.label.starts_with('H'))
        } else {
            registered.iter().find(|x| x.label.starts_with('Z'))
        }
        .unwrap_or(&registered[(i + 1) % registered.len()]);

        registry.dev.set_frequency(registry.channel, elsewhere.hz)?;
        std::thread::sleep(Duration::from_millis(40));
        let p_park = power(buf);

        registry.dev.set_frequency(registry.channel, r.hz)?;
        std::thread::sleep(Duration::from_millis(40));
        let p_set = power(buf);

        registry.dev.set_frequency(registry.channel, elsewhere.hz)?;
        std::thread::sleep(Duration::from_millis(40));
        registry.retune(r.hz as f64);
        std::thread::sleep(Duration::from_millis(40));
        let p_qt = power(buf);

        // The test can only distinguish "recalled correctly" from "stayed
        // parked" when those two states differ in power. When they do not, say
        // so — a silent "ok" here would be pure noise dressed as a pass.
        let contrast = (p_park - p_set).abs();
        let verdict = if contrast < 2.0 {
            inconclusive += 1;
            "INCONCLUSIVE (park and target sound alike)"
        } else if (p_set - p_qt).abs() < contrast / 2.0 {
            ok_n += 1;
            "ok"
        } else {
            bad += 1;
            "MISMATCH — recall did not land on this channel"
        };
        println!(
            "  {:>4}  {:10.3}  {p_park:9.2}  {p_set:9.2}  {p_qt:9.2}   {}",
            r.label,
            r.hz as f64 / 1e6,
            verdict,
        );
    }
    println!(
        "\n  {ok_n} confirmed, {bad} wrong, {inconclusive} inconclusive, of {} channels",
        registered.len()
    );
    if inconclusive > 0 {
        println!(
            "  Inconclusive rows are a limit of this test, not a failure: with no\n  \
             signal to separate two channels, received power cannot tell whether a\n  \
             recall moved the radio. Only channels whose park/target power differ\n  \
             by >2 dB are decidable here."
        );
    }
    Ok(())
}
