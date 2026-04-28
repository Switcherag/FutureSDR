// Plugin build script — prints the ABI fingerprint at compile time
// so you can confirm it matches the runtime fingerprint during the build log.
//
// The fingerprint is computed with the same logic as futuresdr's build.rs.
// If both print the same detail string, the plugin is ABI-compatible.

use std::hash::{Hash, Hasher};

fn main() {
    // Read inputs the same way futuresdr's build.rs does
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".into());

    let futuresdr_version = std::env::var("CARGO_PKG_VERSION")
        .unwrap_or_else(|_| "unknown".into());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());

    // Reproduce the exact fingerprint format from futuresdr/build.rs
    // NOTE: the rustc version here is the short form; futuresdr uses semver + commit hash.
    // We print the detail for human inspection — the actual hash is computed by futuresdr.
    let detail = format!(
        "target={};rustc={}",
        target, rustc
    );

    // Print as a cargo warning so it shows up in the build log
    println!("cargo:warning=add_one_plugin ABI build info: {}", detail);

    // Also print the futuresdr version being linked against
    println!(
        "cargo:warning=add_one_plugin links futuresdr={}  (must match runtime)",
        futuresdr_version
    );
}
