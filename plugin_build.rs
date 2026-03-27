// Shared build script for FutureSDR plugin-loading binaries.
//
// Reference from any binary crate's Cargo.toml:
//   build = "../../plugin_build.rs"      (adjust depth as needed)
//
// Emits two rpaths so the binary finds its .so dependencies at runtime
// without needing LD_LIBRARY_PATH:
//   $ORIGIN          → libfuturesdr.so + plugin .so files (same dir as binary)
//   <sysroot>/lib/…  → libstd-*.so from the Rust toolchain

fn main() {
    // Use RPATH (not RUNPATH) so our paths take priority over LD_LIBRARY_PATH.
    // RUNPATH is checked AFTER LD_LIBRARY_PATH, which causes stale .so copies
    // in shared_libs/ or other directories to shadow the correct ones.
    println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");

    // 1. Look for .so files next to the binary itself.
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");

    // 2. Look for libstd in the Rust toolchain sysroot.
    if let Ok(output) = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
    {
        let sysroot = String::from_utf8_lossy(&output.stdout);
        let sysroot = sysroot.trim();
        if let Ok(target) = std::env::var("TARGET") {
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath,{}/lib/rustlib/{}/lib",
                sysroot, target
            );
        }
    }
}
