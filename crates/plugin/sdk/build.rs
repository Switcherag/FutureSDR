// Record the compiler this crate is built with: the plugins an SDK builds must
// use the same one as the library they link against.
//
// The workspace links the standard library dynamically, so the binary also
// gets a search path to it (and to its own directory).
use std::process::Command;

fn rustc(args: &[&str]) -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    Command::new(rustc)
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn main() {
    println!("cargo:rustc-env=PLUGIN_SDK_RUSTC={}", rustc(&["-V"]));
    println!(
        "cargo:rustc-env=PLUGIN_SDK_TOOLCHAIN={}",
        std::env::var("RUSTUP_TOOLCHAIN").unwrap_or_default()
    );
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");

    let libdir = rustc(&["--print", "target-libdir"]);
    if !libdir.is_empty() {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{libdir}");
    }
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
}
