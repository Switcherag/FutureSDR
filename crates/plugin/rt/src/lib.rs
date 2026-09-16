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

/// Everything a plugin crate usually needs. Replaces FutureSDR's own
/// application prelude for plugin crates.
pub mod prelude {
    pub use futuresdr::blocks;
    pub use futuresdr::num_complex::Complex64;
    pub use futuresdr::runtime::dev::prelude::*;
    pub use plugin_api::FromSetting;
    pub use plugin_api::Settings;
    pub use plugin_api::anyhow;
    pub use plugin_api::export_plugin;
}
