// ziglow_swap_quicktune — dual-PHY swap, retuned by bladeRF quick tune.
//
// `ziglow_swap` with the radio driven by libbladeRF directly instead of
// SoapySDR, so every cross-band swap is a quick-tune profile recall rather
// than a `set_frequency`:
//
//   Z: flows/zigbee_rxA.toml   802.15.4 @ 2.425 GHz (channel 15)
//   H: flows/halow_rxA.toml    802.11ah  @ 919 MHz
//
// This is the case quick tune exists for. Every swap here changes band, and
// measured on this radio a cross-band hop costs:
//
//   host tuning (SoapySDR default)   111.6 ms
//   FPGA tuning                       26.9 ms
//   quick tune                         0.3 ms
//
// so the retune stops being the thing that limits how fast the receiver can
// alternate. Sibling binaries: `ziglow_swap` (same experiment, SoapySDR) and
// `zigbee_swap_quicktune` (quick tune, but both flows in one band).
//
// The CSV schema is identical to `ziglow_swap.csv`, so `plot_ziglow.py` reads
// either without changes and the two can be compared directly.
//
// Why the radio had to change hands: SoapySDR claims the USB device, so a
// second libbladeRF handle cannot open alongside it, and SoapySDR exposes no
// route to `bladerf_schedule_retune`. So this binary owns the radio and feeds
// the flowgraph through `connect_radio` — no head TOML, no seify_source_plugin.
//
// Output: ziglow_swap_quicktune.csv, one row per received frame (and timeout).
// Run from this directory:
//   cd examples/real_device_swap
//   bash build.sh            # builds the whole ecosystem in one invocation
//   ../../target/release/ziglow_swap_quicktune

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

/// `BLADERF_RETUNE_NOW` — a C macro, so bindgen does not emit it.
const RETUNE_NOW: u64 = 0;

/// SC16_Q11 full scale: the AD9361 delivers 12-bit samples left-aligned in 16
/// bits, so +-2048 maps to +-1.0.
const SC16_Q11_SCALE: f32 = 1.0 / 2048.0;

const FLOW_Z: &str = "flows/zigbee_rxA.toml";
const FLOW_H: &str = "flows/halowv6A.toml";
const TAIL_FLOW: &str = "flows/null_tail.toml";
const CSV_PATH: &str = "ziglow_swap_quicktunev2.csv";

/// `[[controller_taps]] name` declared by the Zigbee flow. Which PHY decoded a
/// frame is taken from the tap name, not from the controller's current flow,
/// so a frame landing across a swap boundary is still attributed correctly.
const TAP_Z: &str = "zigbee_frames";

/// Channels registered for quick tune at startup, in MHz.
///
/// A registry, not a schedule: a recall is a lookup by frequency, so either is
/// reachable in ~0.3 ms whenever a flow asks for it. Kept small deliberately —
/// recall latency is flat up to 8 registered profiles and roughly 4x higher
/// from 16 onward, so a working set of two sits on the fast side.
const QUICKTUNE_CHANNELS_MHZ: &[f64] = &[919.0, 2425.0];

/// If no tap frame arrives within this time after the last swap, swap anyway.
const RX_TIMEOUT: Duration = Duration::from_millis(80000);

/// Cap on the shared deque, in samples — matches the controller's own C32
/// bridge capacity so behaviour does not change with the feed path.
const BUF_CAP: usize = 32_768;

/// Source EUI-64 used by the multizig firmware: `00 00 'E' 'E' 'B' 'G' 'I' 'Z'`.
const ZIGBEE_EUI64_ANCHOR: [u8; 8] = [0x00, 0x00, b'E', b'E', b'B', b'G', b'I', b'Z'];

/// What a decoded frame carries, which differs sharply by PHY.
enum Frame {
    /// Z: `step, run, tag, wait_us, ts_us` from the firmware stamp.
    Z(u32, u16, u8, u32, u64),
    /// H: the 802.11 sequence number. RPG payload is pseudo-random, so the
    /// sequence number is the only frame counter available; `None` for a frame
    /// too short or of a type carrying no sequence control.
    H(Option<u16>),
    /// Decoded, but nothing could be read out of it — a Zigbee frame whose
    /// stamp could not be located. Recorded rather than guessed at, and rather
    /// than dropped, so the row count still reflects what the PHY produced.
    Unstamped,
}

#[derive(Parser, Debug)]
#[command(about = "Dual-PHY swap (802.15.4 <-> 802.11ah), retuned by bladeRF quick tune.")]
struct Args {
    /// Register the two channels, report the profiles assigned, and exit.
    #[arg(long)]
    register_only: bool,
    /// Hardware sample rate (Hz). Tracks `sdr_head.toml` so this path and the
    /// SoapySDR one run the same configuration.
    #[arg(long, default_value_t = 20e6)]
    sample_rate: f64,
    /// Software decimation, matching the head's `fir_resampler_plugin` 1:N.
    /// `--sample-rate / --decim` must equal the rate the flows demand.
    #[arg(long, default_value_t = 5)]
    decim: usize,
    /// Samples per USB transfer buffer (multiple of 1024).
    #[arg(long, default_value_t = 4096)]
    stream_buffer: usize,
    /// Number of transfer buffers; must exceed `--stream-transfers`.
    #[arg(long, default_value_t = 16)]
    stream_buffers: u32,
    /// In-flight USB transfers.
    #[arg(long, default_value_t = 8)]
    stream_transfers: u32,
    /// RX gain (dB) at startup. The flows' `[radio] gain_db` overrides this on
    /// the first swap.
    #[arg(long, default_value_t = 10)]
    gain_db: i32,
    /// Use `set_frequency` instead of quick tune — the control for measuring
    /// what quick tune is worth, on otherwise identical machinery.
    #[arg(long)]
    no_quick_tune: bool,
}

/// The channels named in [`QUICKTUNE_CHANNELS_MHZ`].
fn selected_channels() -> Vec<(String, u64)> {
    QUICKTUNE_CHANNELS_MHZ
        .iter()
        .map(|mhz| (format!("{mhz:.0}MHz"), (mhz * 1e6).round() as u64))
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

// ── Frame parsing ────────────────────────────────────────────────────────

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

/// Pull the 802.11 sequence number out of a decoded HaLow MAC frame.
///
/// The tap is on `decoder.rx_frames`, so this is the bare MAC frame with no
/// RFtap header. Sequence control sits at offset 22; the sequence number is
/// its top 12 bits. Control frames (type 1) carry none.
fn parse_halow_seq(frame: &[u8]) -> Option<u16> {
    if frame.len() < 24 || (frame[0] >> 2) & 0x3 == 1 {
        return None;
    }
    Some(u16::from_le_bytes([frame[22], frame[23]]) >> 4)
}

fn other(curr: &str) -> &'static str {
    if curr == FLOW_Z { FLOW_H } else { FLOW_Z }
}

fn phy_name(toml: &str) -> &'static str {
    if toml == FLOW_Z { "Z" } else { "H" }
}

// ── Main ─────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    futuresdr::runtime::init();

    println!("=== ziglow_swap_quicktune — dual-PHY swap, quick-tune retune ===");
    println!("Z: {FLOW_Z} (802.15.4, 2.425 GHz ch15)");
    println!("H: {FLOW_H} (802.11ah, 919 MHz)");

    // ── Radio ────────────────────────────────────────────────────────────
    let dev = Arc::new(BladeRfAny::open_first()?);
    println!("bladeRF {}", dev.get_serial().unwrap_or_default());
    let ch = Channel::Rx0;

    // Tuning mode FIRST: switching it re-initialises the AD9361 and discards
    // the sample rate, bandwidth and gain set before it — the device comes
    // back at its 30.72 MSps default and the PHY silently sees a
    // time-compressed stream.
    dev.set_tuning_mode(TuningMode::FPGA)?;
    let actual_rate = dev.set_sample_rate(ch, args.sample_rate as u32)?;
    let actual_bw = dev.set_bandwidth(ch, args.sample_rate as u32)?;
    dev.set_gain(ch, args.gain_db)?;
    if (actual_rate as f64 - args.sample_rate).abs() > 1.0 {
        return Err(format!(
            "asked for {:.3} MSps, device gave {:.3} MSps",
            args.sample_rate / 1e6,
            actual_rate as f64 / 1e6
        )
        .into());
    }
    println!(
        "  {:.3} MSps hw / {:.3} MSps to the PHY (decim {}), bw {:.3} MHz, gain {} dB, tuning {:?}",
        actual_rate as f64 / 1e6,
        actual_rate as f64 / args.decim.max(1) as f64 / 1e6,
        args.decim,
        actual_bw as f64 / 1e6,
        args.gain_db,
        dev.get_tuning_mode()?,
    );

    let t_reg = Instant::now();
    let (registry, registered) = Registry::register(dev.clone(), ch, &selected_channels())?;
    let registry = Arc::new(registry);
    println!(
        "\nregistered {} channels in {:.1} ms:",
        registered.len(),
        t_reg.elapsed().as_secs_f64() * 1000.0
    );
    for r in &registered {
        println!(
            "  {:>8}  {:9.3} MHz   nios_profile={:<4} rffe_profile={}",
            r.label,
            r.hz as f64 / 1e6,
            r.nios_profile,
            r.rffe_profile
        );
    }
    if args.register_only {
        return Ok(());
    }

    // ── Stream ───────────────────────────────────────────────────────────
    let buf: plugin_host::RadioOutputBuf = Arc::new(Mutex::new(Default::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let overruns = Arc::new(AtomicU64::new(0));
    let pushed = Arc::new(AtomicU64::new(0));
    let peak_ppm = Arc::new(AtomicU64::new(0));
    // Gates the reader the way a BridgeSink's `connected` flag gates the head
    // flowgraph: closed across a swap so the incoming flowgraph is not handed
    // a spliced stream.
    let feeding = Arc::new(AtomicBool::new(true));

    let reader = {
        let (dev, buf, stop, overruns) =
            (dev.clone(), buf.clone(), stop.clone(), overruns.clone());
        let (pushed, peak_ppm, feeding) = (pushed.clone(), peak_ppm.clone(), feeding.clone());
        let (decim, stream_buffer) = (args.decim.max(1), args.stream_buffer);
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

            // The same filter the head builds: FirBuilder::resampling(1, decim)
            // designs kaiser::multirate(1, decim, 12, 0.0001).
            let taps: Vec<f32> = firdes::kaiser::multirate(1, decim, 12, 0.0001);
            let n_hist = taps.len().saturating_sub(1);
            let fir = DecimatingFirFilter::<Complex32, Complex32, Vec<f32>>::new(decim, taps);
            let mut hist: Vec<Complex32> = vec![Complex32::new(0.0, 0.0); n_hist];
            let mut scratch: Vec<Complex32> = Vec::with_capacity(raw.len() + n_hist);
            let mut out: Vec<Complex32> = Vec::new();

            while !stop.load(Ordering::Relaxed) {
                if rx.read(&mut raw, Duration::from_millis(200)).is_err() {
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
                    let (_consumed, produced_n, _) = fir.filter(&scratch, &mut out);
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
                let mut q = buf.lock().unwrap();
                let overflow = (q.len() + produced.len()).saturating_sub(BUF_CAP);
                if overflow > 0 {
                    q.drain(..overflow);
                    overruns.fetch_add(overflow as u64, Ordering::Relaxed);
                }
                q.extend(produced.iter().copied());
            }
            rx.disable()?;
            Ok(())
        })
    };
    std::thread::sleep(Duration::from_millis(200));

    // Registration leaves the radio on whichever channel was registered last,
    // so park it where the initial flow expects. Without this the receiver
    // starts deaf and cannot recover: a swap is what retunes, and a swap only
    // happens once a frame arrives.
    let start_hz = flow_frequency_hz(FLOW_Z)?;
    registry.retune(start_hz as f64);
    println!("\nInitial PHY: Z @ {:.3} MHz\nCSV → {CSV_PATH}\n", start_hz as f64 / 1e6);

    // ── Flowgraphs ───────────────────────────────────────────────────────
    // No head TOML: the radio is ours, and `connect_radio` feeds the
    // swappable's input port straight from the deque the reader fills.
    let swap_idx = 1usize;
    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_permanent(TAIL_FLOW)
        .add_swappable(FLOW_Z)
        .connect_radio(buf.clone(), swap_idx, "in")
        .tap_channel(256);

    let quick = !args.no_quick_tune;
    let reg_for_setter = registry.clone();
    let dev_for_gain = dev.clone();
    let feeding_ctl = feeding.clone();
    let buf_ctl = buf.clone();

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
        // frequency demand. Quick tune makes it ~0.3 ms even across bands.
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
        // Same schema as ziglow_swap.csv, so plot_ziglow.py reads either.
        // `phy` is the PHY that decoded the frame (from the tap name); `len` is
        // the one field both PHYs always provide. Stamp columns are Z-only,
        // `seq` is H-only; unavailable fields are -1.
        writeln!(
            csv,
            "rx_idx,phy,tap,frame_event,len,step,run,tag,wait_us,ts_us,seq,rx_t_ms,swap_ms"
        )?;
        csv.flush().ok();

        // Signal check every 5 s, so silence is never ambiguous: it separates
        // "no signal" from "no samples" from "dead reader".
        {
            let (buf, overruns, pushed, peak_ppm) =
                (buf.clone(), overruns.clone(), pushed.clone(), peak_ppm.clone());
            std::thread::spawn(move || {
                let mut last = 0u64;
                loop {
                    std::thread::sleep(Duration::from_secs(5));
                    let np = pushed.load(Ordering::Relaxed);
                    let peak = peak_ppm.swap(0, Ordering::Relaxed) as f64 / 1e6;
                    println!(
                        "  [radio] {:.3} MSps in, deque {}, peak {:.4} ({:.1} dBFS), {} dropped",
                        (np - last) as f64 / 5.0 / 1e6,
                        buf.lock().unwrap().len(),
                        peak,
                        20.0 * peak.max(1e-9).log10(),
                        overruns.load(Ordering::Relaxed),
                    );
                    last = np;
                }
            });
        }

        let mut current = FLOW_Z;
        let mut rx_idx: u64 = 0;
        let mut swap_total_ms: f64 = 0.0;
        let mut swap_count: u64 = 0;
        let (mut n_z, mut n_h) = (0u64, 0u64);
        let t0 = Instant::now();

        println!(
            "\nReceiver running ({}). Every decoded frame triggers a cross-band swap.",
            if quick { "quick tune" } else { "set_frequency" }
        );
        println!("Press Ctrl-C to stop and finalize CSV.\n");

        loop {
            let mut deadline = FutureExt::fuse(Timer::after(RX_TIMEOUT));
            let frame_event;
            let mut row: Option<(&'static str, String, usize, Frame)> = None;

            select! {
                _ = deadline => {
                    frame_event = "timeout";
                    println!(
                        "[t={:.1}ms] [timeout] no frame on {} — swapping anyway",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        phy_name(current),
                    );
                }
                msg = tap_rx.next().fuse() => match msg {
                    Some((tap_name, pmt)) => {
                        frame_event = "rx";
                        let blob = match pmt {
                            Pmt::Blob(b) => b,
                            other => {
                                println!("[tap {tap_name}] non-blob pmt: {other:?}");
                                continue;
                            }
                        };
                        // The tap name says which PHY produced this, even if
                        // the controller has already moved to the other flow.
                        if tap_name == TAP_Z {
                            n_z += 1;
                            let frame = match parse_payload(&blob) {
                                Some((step, run, tag, wait_us, ts)) => {
                                    Frame::Z(step, run, tag, wait_us, ts)
                                }
                                None => Frame::Unstamped,
                            };
                            row = Some(("Z", tap_name, blob.len(), frame));
                        } else {
                            n_h += 1;
                            row = Some(("H", tap_name, blob.len(),
                                        Frame::H(parse_halow_seq(&blob))));
                        }
                    }
                    None => break,
                },
            }

            let rx_t_ms = t0.elapsed().as_secs_f64() * 1000.0;

            // Swap to the other PHY. This one really does change band.
            let next = other(current);
            let t_swap = Instant::now();
            feeding_ctl.store(false, Ordering::Relaxed);
            let swap_res = ctrl.swap(swap_target, next, &rt_handle).await;
            buf_ctl.lock().unwrap().clear();
            feeding_ctl.store(true, Ordering::Relaxed);
            let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
            if let Err(e) = swap_res {
                eprintln!("[swap] failed: {e}");
                continue;
            }
            swap_total_ms += swap_ms;
            swap_count += 1;
            current = next;

            let (phy, tap, len, step, run, tag, wait, ts, seq) = match &row {
                Some((phy, tap, len, Frame::Z(s, r, g, w, t))) => (
                    *phy, tap.clone(), len.to_string(), s.to_string(), r.to_string(),
                    g.to_string(), w.to_string(), t.to_string(), "-1".to_string(),
                ),
                Some((phy, tap, len, Frame::Unstamped)) => (
                    *phy, tap.clone(), len.to_string(),
                    "-1".into(), "-1".into(), "-1".into(), "-1".into(), "-1".into(),
                    "-1".to_string(),
                ),
                Some((phy, tap, len, Frame::H(seq))) => (
                    *phy, tap.clone(), len.to_string(),
                    "-1".into(), "-1".into(), "-1".into(), "-1".into(), "-1".into(),
                    seq.map_or_else(|| "-1".into(), |s| s.to_string()),
                ),
                None => (
                    "?", String::new(), "-1".into(), "-1".into(), "-1".into(),
                    "-1".into(), "-1".into(), "-1".into(), "-1".into(),
                ),
            };

            writeln!(
                csv,
                "{rx_idx},{phy},{tap},{frame_event},{len},{step},{run},{tag},{wait},{ts},{seq},{rx_t_ms:.3},{swap_ms:.3}",
            )?;
            csv.flush().ok();
            rx_idx += 1;

            if swap_count > 0 && swap_count % 100 == 0 {
                println!(
                    "[stats] {swap_count} swaps, avg swap {:.3} ms, Z={n_z} H={n_h}, {} quick-tune misses",
                    swap_total_ms / swap_count as f64,
                    registry.misses.load(Ordering::Relaxed),
                );
            }
        }

        stop.store(true, Ordering::Relaxed);
        let _ = &reader;
        if swap_count > 0 {
            println!(
                "\nFinal: {swap_count} swaps, avg swap {:.3} ms, Z={n_z} H={n_h}",
                swap_total_ms / swap_count as f64,
            );
        }
        Ok(())
    })
}
