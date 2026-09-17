//! `fsdr-plugin`: pack an SDK, and build plugins against it.
//!
//! ```text
//! fsdr-plugin pack  --from <libfuturesdr_plugin_rt.so> --out <dir>
//! fsdr-plugin build --sdk <dir> <plugin crate dir> [--target-dir <dir>]
//!                   [--clippy] [--deny-warnings]
//! fsdr-plugin info  --sdk <dir>
//! ```
//!
//! Pack from the library your host is built with, e.g. after
//! `cargo build --release` of the host: `target/release/libfuturesdr_plugin_rt.so`.

use std::path::PathBuf;

use anyhow::Result;
use anyhow::bail;
use plugin_sdk::BuildOptions;
use plugin_sdk::Sdk;

const USAGE: &str = "\
usage:
  fsdr-plugin pack  --from <libfuturesdr_plugin_rt.so> --out <dir>
  fsdr-plugin build --sdk <dir> <plugin crate dir> [--target-dir <dir>]
                    [--clippy] [--deny-warnings]
  fsdr-plugin info  --sdk <dir>";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        bail!("{USAGE}");
    };
    if command == "-h" || command == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    let mut out = None;
    let mut sdk = None;
    let mut from = None;
    let mut target_dir = None;
    let mut options = BuildOptions::default();
    let mut positional = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--sdk" => sdk = Some(PathBuf::from(value()?)),
            "--from" => from = Some(PathBuf::from(value()?)),
            "--target-dir" => target_dir = Some(PathBuf::from(value()?)),
            "--clippy" => options.clippy = true,
            "--deny-warnings" => options.deny_warnings = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ if arg.starts_with('-') => bail!("unknown option {arg}\n{USAGE}"),
            _ => positional.push(PathBuf::from(arg)),
        }
    }

    match command.as_str() {
        "pack" => {
            let (Some(from), Some(out)) = (from, out) else {
                bail!("pack needs --from and --out\n{USAGE}")
            };
            let packed = Sdk::of_library(&from)?.pack(&out)?;
            println!(
                "SDK in {} ({}, {:?})",
                out.display(),
                packed.rustc,
                packed.profile
            );
        }
        "build" => {
            let (Some(dir), [crate_dir]) = (sdk, positional.as_slice()) else {
                bail!("build needs --sdk and one plugin crate\n{USAGE}");
            };
            let sdk = Sdk::open(&dir)?;
            let target_dir = target_dir.unwrap_or_else(|| crate_dir.join("target"));
            let library =
                sdk.build_plugin_with(&crate_dir.join("Cargo.toml"), &target_dir, &options)?;
            println!("{}", library.display());
        }
        "info" => {
            let Some(dir) = sdk else {
                bail!("info needs --sdk\n{USAGE}")
            };
            let sdk = Sdk::open(&dir)?;
            println!("library   {}", sdk.rt.display());
            for dir in &sdk.deps {
                println!("crates    {}", dir.display());
            }
            println!("profile   {:?}", sdk.profile);
            println!("toolchain {}", sdk.toolchain);
            println!("rustc     {}", sdk.rustc);
        }
        _ => bail!("unknown command {command}\n{USAGE}"),
    }
    Ok(())
}
