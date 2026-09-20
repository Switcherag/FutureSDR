//! One shared copy of FutureSDR and the plugin API.
//!
//! A host and all the plugins it loads must agree on every type they pass to
//! each other, which in Rust means sharing one compiled copy of the crates
//! that define them. This `dylib` is that copy: hosts depend on it, and
//! plugins are compiled against a particular build of it (see
//! `futuresdr-plugin-sdk`).
//!
//! It re-exports FutureSDR at its root, so a plugin can name it `futuresdr`,
//! which is what `#[derive(Block)]` expects:
//!
//! ```ignore
//! extern crate futuresdr_plugin_rt as futuresdr;
//! use futuresdr::prelude::*;
//!
//! export_plugin! { name: "mine", blocks: [ /* ... */ ] }
//! ```

pub use futuresdr::*;
pub use plugin_api;

pub mod buffer;
/// FFT plans of the library FutureSDR's `Fft` block uses, `rustfft`, made
/// by this library: planning in a plugin would compile all of `rustfft`'s
/// algorithms into it.
pub mod fft {
    use std::sync::Arc;

    pub use rustfft::Fft;

    /// A forward FFT of `len` points on `Complex32`.
    pub fn forward(len: usize) -> Arc<dyn Fft<f32>> {
        rustfft::FftPlanner::new().plan_fft_forward(len)
    }

    /// An inverse FFT of `len` points on `Complex32`, not normalized.
    pub fn inverse(len: usize) -> Arc<dyn Fft<f32>> {
        rustfft::FftPlanner::new().plan_fft_inverse(len)
    }
}

/// Everything a plugin crate usually needs. Replaces FutureSDR's own
/// application prelude for plugin crates.
pub mod prelude {
    pub use crate::buffer::ReuseCpuReader;
    /// The stream ports of a plugin's blocks, in place of FutureSDR's own
    /// alias of the same name: the same circular buffer, with the ring kept
    /// for the next flowgraph (see [`crate::buffer`]). A host that replaces
    /// flowgraphs pays no mapping for a buffer it has had before. Blocks
    /// that take their buffers as type parameters, as FutureSDR's do, are
    /// given `ReuseCpuReader`/`ReuseCpuWriter` explicitly: their defaults
    /// are FutureSDR's buffer, which does not connect to this one.
    pub use crate::buffer::ReuseCpuReader as DefaultCpuReader;
    pub use crate::buffer::ReuseCpuWriter;
    pub use crate::buffer::ReuseCpuWriter as DefaultCpuWriter;
    pub use futuresdr::blocks;
    pub use futuresdr::num_complex::Complex64;
    pub use futuresdr::runtime::dev::prelude::*;
    pub use plugin_api::Added;
    pub use plugin_api::FromSetting;
    pub use plugin_api::Settings;
    pub use plugin_api::anyhow;
    pub use plugin_api::export_plugin;
}
