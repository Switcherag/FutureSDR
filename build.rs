use rustc_version::Channel;
use rustc_version::version_meta;
use std::hash::{Hash, Hasher};

fn main() {
    let meta = version_meta().unwrap();

    match meta.channel {
        Channel::Stable => println!("cargo:rustc-cfg=RUSTC_IS_STABLE"),
        Channel::Beta => println!("cargo:rustc-cfg=RUSTC_IS_BETA"),
        Channel::Nightly => println!("cargo:rustc-cfg=RUSTC_IS_NIGHTLY"),
        Channel::Dev => println!("cargo:rustc-cfg=RUSTC_IS_DEV"),
    }

    // === ABI Fingerprint ===
    // Captures everything that affects binary compatibility between
    // libfuturesdr.so, the runtime binary, and plugin .so files.

    let rustc_version = format!(
        "{}.{}.{}{}",
        meta.semver.major,
        meta.semver.minor,
        meta.semver.patch,
        meta.commit_hash
            .as_deref()
            .map(|h| format!(" ({})", h))
            .unwrap_or_default()
    );

    let futuresdr_version = env!("CARGO_PKG_VERSION");
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());

    // ABI-affecting features: seify changes the Error enum variant count.
    // After the fix (variants always present), this is informational but
    // still useful for diagnosing mismatches.
    let has_seify = cfg!(feature = "seify");

    // Build a deterministic fingerprint string
    let fingerprint_source = format!(
        "rustc={};futuresdr={};target={};seify={}",
        rustc_version, futuresdr_version, target, has_seify
    );

    // Hash it for a compact representation
    let mut hasher = std::hash::DefaultHasher::new();
    fingerprint_source.hash(&mut hasher);
    let hash = hasher.finish();

    // Emit both the readable source and the hash
    println!(
        "cargo:rustc-env=FUTURESDR_ABI_FINGERPRINT={:016x}",
        hash
    );
    println!(
        "cargo:rustc-env=FUTURESDR_ABI_FINGERPRINT_DETAIL={}",
        fingerprint_source
    );
}
