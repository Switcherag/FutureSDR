// record_iq — plain radio head into a file sink, one PHY at a time.
//
// Step 1 of the replay pipeline. There is no plugin, no controller and no
// swap here on purpose: the only job is to put an unmodified 4 MSps baseband
// capture of ONE standard on disk, so a single clean frame can be cut out of
// it by hand and reused forever after.
//
//     SeifySource(freq, 4 MSps, gain) -> Head(duration * rate) -> FileSink
//
// The rate matches what every flow in `flows/` demands (4 MSps), so a cut
// frame can be replayed into those flows without resampling. `--phy` presets
// the centre frequency to the same value the corresponding flow TOML asks
// for, which is the only thing that has to agree for the capture to decode:
//
//     zigbee   2.425 GHz   flows/zigbee_rxA.toml   (802.15.4 channel 11)
//     halow    919.0 MHz   flows/halowv6A.toml     (S1G channel 34, 2 MHz)
//
// Format is cf32 — interleaved little-endian f32 I,Q, i.e. 8 bytes per
// sample — the native wire format of `FileSink<Complex32>` and what the
// splicer and the file-source replay head read back. NOT the sc16 used by
// `freq_swap/quicktune_swap_iq.rs`; nothing here talks to libbladeRF
// directly, so there is no reason to carry the Q11 scaling around.
//
// A sidecar `<output>.meta.json` records rate/frequency/gain/length next to
// the samples, so a capture is still self-describing months later and the
// splicer can refuse to mix two recordings made at different rates.
//
// Run from this directory:
//     cd examples/real_device_swap
//     ../../target/release/record_iq --phy zigbee --duration 10
//     ../../target/release/record_iq --phy halow  --duration 10 --gain 10
//
// Cropping the result down to a single frame is done by hand; the splicer
// that builds the alternating-PHY stream takes the cropped cf32 files.

use std::fs;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::Head;
use futuresdr::prelude::*;

/// Which standard is being captured. Only sets the default centre frequency
/// and the default output name — the capture itself is PHY-agnostic.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Phy {
    /// 802.15.4 @ 2.425 GHz, as demanded by flows/zigbee_rxA.toml.
    Zigbee,
    /// 802.11ah @ 919 MHz, as demanded by flows/halowv6A.toml.
    Halow,
}

impl Phy {
    /// Centre frequency the matching flow TOML declares in its `[radio]`.
    fn freq_hz(self) -> f64 {
        match self {
            Phy::Zigbee => 2.425e9,
            Phy::Halow => 919.0e6,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Phy::Zigbee => "zigbee",
            Phy::Halow => "halow",
        }
    }
}

#[derive(Parser, Debug)]
#[command(about = "Record raw cf32 baseband from the radio head to a file, one PHY at a time.")]
struct Args {
    /// Which standard to record. Presets the centre frequency.
    #[arg(long, value_enum)]
    phy: Phy,

    /// Output cf32 path. Defaults to `recording/<phy>_raw.cf32`.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Centre frequency in Hz. Overrides the `--phy` preset.
    #[arg(long)]
    freq: Option<f64>,

    /// Sample rate in samples/second. Every flow in `flows/` wants 4e6;
    /// changing it means the capture cannot be replayed into them as-is.
    #[arg(long, default_value_t = 4e6)]
    sample_rate: f64,

    /// RF gain in dB.
    #[arg(long, default_value_t = 0.0)]
    gain: f64,

    /// Recording duration in seconds.
    #[arg(long, default_value_t = 10.0)]
    duration: f64,

    /// Seify device args, e.g. "driver=bladerf".
    #[arg(long)]
    args: Option<String>,

    /// Antenna name, if the device needs one selected.
    #[arg(long)]
    antenna: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let freq = args.freq.unwrap_or_else(|| args.phy.freq_hz());
    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from(format!("recording/{}_raw.cf32", args.phy.tag())));

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    // Head takes a sample count, so the file length is decided up front
    // rather than by when the operator hits Ctrl-C. That keeps the sidecar's
    // `samples` field truthful and makes the capture reproducible.
    let n_samples = (args.duration * args.sample_rate).round() as u64;

    println!(
        "recording {} @ {:.6} MHz, {:.3} MSps, gain {} dB",
        args.phy.tag(),
        freq / 1e6,
        args.sample_rate / 1e6,
        args.gain,
    );
    println!(
        "  {n_samples} samples ({:.1} s, {:.1} MiB cf32) -> {}",
        args.duration,
        (n_samples as f64 * 8.0) / (1024.0 * 1024.0),
        output.display(),
    );

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    let src = Builder::new(args.args.clone())?
        .frequency(freq)
        .sample_rate(args.sample_rate)
        .gain(args.gain)
        .antenna(args.antenna.clone())
        .build_source()?;

    connect!(fg, src);
    let src_id: BlockId = src.into();
    let head = fg.add_block(Head::<Complex32>::new(n_samples));
    let sink = fg.add_block(FileSink::<Complex32>::new(
        output.to_string_lossy().to_string(),
    ));

    fg.connect_dyn(src_id, "outputs[0]", &head, "input")?;
    fg.connect_dyn(head, "output", &sink, "input")?;

    rt.run(fg)?;

    // Sidecar, written only once the capture actually completed, so a
    // half-written .cf32 is never accompanied by a meta claiming it is whole.
    let meta = format!(
        concat!(
            "{{\n",
            "  \"phy\": \"{}\",\n",
            "  \"format\": \"cf32\",\n",
            "  \"center_freq_hz\": {},\n",
            "  \"sample_rate_hz\": {},\n",
            "  \"gain_db\": {},\n",
            "  \"samples\": {},\n",
            "  \"duration_s\": {}\n",
            "}}\n",
        ),
        args.phy.tag(),
        freq,
        args.sample_rate,
        args.gain,
        n_samples,
        args.duration,
    );
    let meta_path = output.with_extension("meta.json");
    fs::write(&meta_path, meta)?;

    println!("done: {} + {}", output.display(), meta_path.display());
    Ok(())
}
