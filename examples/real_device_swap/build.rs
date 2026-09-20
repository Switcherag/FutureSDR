//! Where libbladeRF is, from pkg-config: a libbladeRF built from source
//! installs to /usr/local, which the linker does not search.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_BLADERF").is_none() {
        return;
    }
    let Ok(out) = std::process::Command::new("pkg-config")
        .args(["--libs-only-L", "libbladeRF"])
        .output()
    else {
        return;
    };
    for dir in String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|flag| flag.strip_prefix("-L"))
    {
        println!("cargo::rustc-link-search=native={dir}");
        // And for the loader, in case the directory is not in its cache.
        println!("cargo::rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
