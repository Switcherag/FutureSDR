//! SDR front ends through seify, as a plugin: FutureSDR's `SeifySource` and
//! `SeifySink`, configured by settings.
//!
//! ```toml
//! [blocks.src]
//! type = "SeifySource"
//! args = "driver=dummy"     # seify device arguments, e.g. "soapy=bladerf"
//! frequency = 919.0e6
//! sample_rate = 4e6
//! gain = 30
//!
//! [outputs]
//! samples = "src.outputs[0]"
//!
//! [controls]                # what receivers may ask for in [radio]
//! frequency = "src.freq"
//! gain = "src.gain"
//! sample_rate = "src.sample_rate"
//! ```
//!
//! Drivers other than `dummy` must be enabled in the shared library the
//! SDK was packed from (feature `soapy` of `futuresdr-plugin-rt`).

extern crate futuresdr_plugin_rt as futuresdr;

use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;
use futuresdr::seify::DynDevice;

/// A seify builder from the settings: `args`, and optionally `frequency`,
/// `sample_rate`, `gain`, `bandwidth` (numbers), `antenna` (a name) and
/// `channels` (a list of indices, default `[0]`).
fn builder(s: &Settings) -> anyhow::Result<Builder<DynDevice>> {
    let args: String = s.get_or("args", String::new())?;
    let mut builder = Builder::new(args.as_str())?;
    if let Some(f) = s.get_opt("frequency")? {
        builder = builder.frequency(f);
    }
    if let Some(r) = s.get_opt("sample_rate")? {
        builder = builder.sample_rate(r);
    }
    if let Some(g) = s.get_opt("gain")? {
        builder = builder.gain(g);
    }
    if let Some(b) = s.get_opt("bandwidth")? {
        builder = builder.bandwidth(b);
    }
    if let Some(a) = s.get_opt::<String>("antenna")? {
        builder = builder.antenna(a);
    }
    if let Some(c) = s.get_opt::<Vec<usize>>("channels")? {
        builder = builder.channels(c);
    }
    Ok(builder)
}

export_plugin! {
    name: "radio",
    blocks: [
        {
            name: "SeifySource",
            description: "Receive Complex32 samples on `outputs[0]`, ... (settings args, frequency, \
                          sample_rate, gain, bandwidth, antenna, channels); message inputs freq, \
                          gain, sample_rate, ...",
            add: |s| builder(s)?.build_source()?,
        },
        {
            name: "SeifySink",
            description: "Transmit Complex32 samples from `inputs[0]`, ... (settings as for \
                          SeifySource).",
            add: |s| builder(s)?.build_sink()?,
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use futuresdr::blocks::NullSink;

    use super::*;

    fn settings(values: &[(&str, Pmt)]) -> Settings {
        let values: HashMap<String, Pmt> = values
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        Settings::new("src", values)
    }

    fn dummy(frequency: f64) -> Settings {
        settings(&[
            ("args", Pmt::String("driver=dummy".into())),
            ("frequency", Pmt::F64(frequency)),
            ("sample_rate", Pmt::F64(4e6)),
            ("gain", Pmt::Isize(10)),
            ("channels", Pmt::VecPmt(vec![Pmt::Isize(0)])),
        ])
    }

    #[test]
    fn a_dummy_source_streams_and_retunes() -> Result<()> {
        let plugin = futuresdr_plugin_entry();
        let source = plugin
            .blocks
            .iter()
            .find(|b| b.name == "SeifySource")
            .unwrap();
        let mut fg = Flowgraph::new();
        let src = (source.add)(&mut fg, &dummy(919e6))?;
        let snk = fg.add(NullSink::<Complex32>::new())?;
        fg.stream_dyn(src.id, "outputs[0]", snk.id(), "input")?;

        let running = Runtime::new().start(fg)?;
        let (task, handle) = running.split();
        let freq = |p| block_on(handle.call(src.id, "freq", p));
        assert_eq!(freq(Pmt::Null)?, Pmt::F64(919e6));
        assert_eq!(freq(Pmt::F64(868.3e6))?, Pmt::Ok);
        assert_eq!(freq(Pmt::Null)?, Pmt::F64(868.3e6));
        assert_eq!(freq(Pmt::String("x".into()))?, Pmt::InvalidValue);
        std::thread::sleep(std::time::Duration::from_millis(20));
        block_on(handle.stop())?;
        let done = block_on(task)?;
        assert!(done.block(&snk)?.n_received() > 0);
        Ok(())
    }

    #[test]
    fn mistakes_are_reported() {
        let plugin = futuresdr_plugin_entry();
        let add = |name: &str, s: &Settings| {
            let block = plugin.blocks.iter().find(|b| b.name == name).unwrap();
            (block.add)(&mut Flowgraph::new(), s)
        };
        assert!(
            add(
                "SeifySource",
                &settings(&[("args", Pmt::String("driver=nope".into()))])
            )
            .is_err()
        );
        assert!(add("SeifySink", &settings(&[("args", Pmt::Bool(true))])).is_err());
        assert!(add("SeifySink", &dummy(2.4e9)).is_ok());
    }
}
