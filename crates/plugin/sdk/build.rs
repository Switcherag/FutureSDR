// Record the compiler this crate is built with: the plugins an SDK builds must
// use the same one as the library they link against.
use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(&rustc)
        .arg("-V")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=PLUGIN_SDK_RUSTC={}", version.trim());
    println!(
        "cargo:rustc-env=PLUGIN_SDK_TOOLCHAIN={}",
        std::env::var("RUSTUP_TOOLCHAIN").unwrap_or_default()
    );
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
}
