//! ABI Fingerprint for Plugin Compatibility
//!
//! The fingerprint captures everything that affects binary compatibility
//! between `libfuturesdr.so` and dynamically loaded plugin `.so` files:
//! - Rust compiler version (exact nightly build)
//! - FutureSDR crate version
//! - Target triple
//! - ABI-affecting feature flags
//!
//! Plugins embed the fingerprint at compile time via `export_plugin!`.
//! `LoadedPlugin::load()` validates it against the runtime's fingerprint
//! before using the plugin.

/// Compact ABI fingerprint hash (16 hex chars).
///
/// Two builds produce the same hash only if they used the same compiler,
/// the same futuresdr version, the same target, and the same ABI-affecting
/// features.
pub const ABI_FINGERPRINT: &str = env!("FUTURESDR_ABI_FINGERPRINT");

/// Human-readable detail string showing what went into the fingerprint.
///
/// Format: `rustc=<version>;futuresdr=<version>;target=<triple>;seify=<bool>`
pub const ABI_FINGERPRINT_DETAIL: &str = env!("FUTURESDR_ABI_FINGERPRINT_DETAIL");
