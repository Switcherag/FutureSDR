// Delay plugin for Complex32 streams.
//
// Config: usize — delay in samples

use futuresdr::blocks::Delay;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::{DefaultCpuReader, DefaultCpuWriter};

plugin_api::export_plugin! {
    name: "DelayComplex",
    description: "Sample delay for Complex32 streams",
    config: isize,
    create: |cfg, _id| {
        Delay::<Complex32, DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new(cfg)
    }
}
