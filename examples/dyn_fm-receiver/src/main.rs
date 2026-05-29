// Dynamic FM receiver — SeifySource is used directly from the library,
// all other blocks are loaded as plugins at runtime.
//
// Required plugins (shared libraries):
//   - libfreq_shift_plugin.so     config: (f64, f64)
//   - libfir_resampler_plugin.so  config: (usize, usize)
//   - libfm_demod_plugin.so       config: ()
//   - libfir_resampler_real_plugin.so  config: (usize, usize, f64, f64, f64)
//   - libaudio_sink_plugin.so     config: (u32, u16)

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io;
use futuresdr::blocks::audio::AudioSink;
use futuresdr::blocks::seify::Builder;
use futuresdr::num_integer::gcd;
use futuresdr::runtime::{Flowgraph, Pmt, Runtime};
use plugin_host::LoadedPlugin;

#[derive(Parser, Debug)]
struct Args {
    /// Gain to apply to the seify source
    #[clap(short, long, default_value_t = 30.0)]
    gain: f64,

    /// Center frequency
    #[clap(short, long, default_value_t = 100_000_000.0)]
    frequency: f64,

    /// Sample rate
    #[clap(short, long, default_value_t = 1000000.0)]
    rate: f64,

    /// Seify args
    #[clap(short, long, default_value = "")]
    args: String,

    /// Multiplier for intermediate sample rate
    #[clap(long)]
    audio_mult: Option<u32>,

    /// Audio Rate
    #[clap(long)]
    audio_rate: Option<u32>,

    /// Directory containing plugin .so files
    #[clap(long, default_value = ".")]
    plugin_dir: String,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let args = Args::parse();
    println!("Configuration {args:?}");

    let sample_rate = args.rate as u32;
    let freq_offset = args.rate / 4.0;
    println!("Frequency Offset {freq_offset:?}");

    let audio_rate = if let Some(r) = args.audio_rate {
        r
    } else {
        let mut audio_rates = AudioSink::supported_sample_rates();
        assert!(!audio_rates.is_empty());
        audio_rates.sort_by_key(|a| std::cmp::Reverse(gcd(*a, sample_rate)));
        println!("Supported Audio Rates {audio_rates:?}");
        audio_rates[0]
    };
    println!("Selected Audio Rate {audio_rate:?}");

    let audio_mult = if let Some(m) = args.audio_mult {
        m
    } else {
        let mut m = 5;
        while (m * audio_rate) as f64 > freq_offset + 100e3 {
            m -= 1;
        }
        m
    };
    println!("Audio Mult {audio_mult:?}");

    // --- Load plugins ---
    let dir = &args.plugin_dir;

    let freq_shift = unsafe {
        LoadedPlugin::load(&format!("{dir}/libfreq_shift_plugin.so"))
    };
    let fir_resampler = unsafe {
        LoadedPlugin::load(&format!("{dir}/libfir_resampler_plugin.so"))
    };
    let fm_demod = unsafe {
        LoadedPlugin::load(&format!("{dir}/libfm_demod_plugin.so"))
    };
    let fir_resampler_real = unsafe {
        LoadedPlugin::load(&format!("{dir}/libfir_resampler_real_plugin.so"))
    };
    let audio_sink = unsafe {
        LoadedPlugin::load(&format!("{dir}/libaudio_sink_plugin.so"))
    };

    println!("All plugins loaded successfully");

    // --- Build flowgraph ---
    let mut fg = Flowgraph::new();

    // SeifySource: used directly from the library (not a plugin)
    let src = Builder::new(args.args)?
        .frequency(args.frequency + freq_offset)
        .sample_rate(args.rate)
        .gain(args.gain)
        .build_source()?;

    // FreqShift: (frequency_offset_hz, sample_rate_hz)
    let shift = fg.add_block_dyn(freq_shift.prepare(Box::new((
        freq_offset,
        args.rate,
    ))));

    // FirResamplerComplex: (interp, decim)
    let interp = (audio_rate * audio_mult) as usize;
    let decim = sample_rate as usize;
    println!("interp {interp}   decim {decim}");
    let resamp1 = fg.add_block_dyn(fir_resampler.prepare(Box::new((interp, decim))));

    // FmDemod: ()
    let demod = fg.add_block_dyn(fm_demod.prepare(Box::new(())));

    // FirResamplerReal: (interp, decim, cutoff, transition, attenuation)
    let cutoff = 2_000.0 / (audio_rate * audio_mult) as f64;
    let transition = 10_000.0 / (audio_rate * audio_mult) as f64;
    println!("cutoff {cutoff}   transition {transition}");
    let resamp2 = fg.add_block_dyn(fir_resampler_real.prepare(Box::new((
        1_usize,
        audio_mult as usize,
        cutoff,
        transition,
        0.1_f64,
    ))));

    // AudioSink: (sample_rate, channels)
    let snk = fg.add_block_dyn(audio_sink.prepare(Box::new((audio_rate, 1_u16))));

    // --- Connect: src > shift > resamp1 > demod > resamp2 > snk ---
    let src = fg.add_block(src);
    let src_id = src.get()?.id;
    fg.connect_dyn(src_id, "outputs[0]", shift, "input")?;
    fg.connect_dyn(shift, "output", resamp1, "input")?;
    fg.connect_dyn(resamp1, "output", demod, "input")?;
    fg.connect_dyn(demod, "output", resamp2, "input")?;
    fg.connect_dyn(resamp2, "output", snk, "input")?;

    // Start the flowgraph
    let rt = Runtime::new();
    let (_res, mut handle) = rt.start_sync(fg)?;

    // Keep asking user for a new frequency
    loop {
        println!("Enter a new frequency (in MHz)");
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .expect("error: unable to read user input");
        input.retain(|c| !c.is_whitespace());

        if let Ok(new_freq) = input.parse::<f64>() {
            println!("Setting frequency to {input}");
            async_io::block_on(handle.call(src_id, "freq", Pmt::F64(new_freq * 1e6 + freq_offset)))?;
        } else {
            println!("Input not parsable: {input}");
        }
    }
}
