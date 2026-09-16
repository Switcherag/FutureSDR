//! `fsdr-plugin`: pack an SDK, and build plugins against it.
//!
//! ```text
//! fsdr-plugin pack  --out <dir> [--release] [--workspace <crates/plugin/Cargo.toml>]
//! fsdr-plugin build --sdk <dir> <plugin crate dir> [--target-dir <dir>]
//! fsdr-plugin info  --sdk <dir>
//! ```

use std::path::PathBuf;

use anyhow::Result;
use anyhow::bail;
use plugin_sdk::Profile;
use plugin_sdk::Sdk;

const USAGE: &str = "\
usage:
  fsdr-plugin pack  --out <dir> [--release] [--workspace <Cargo.toml>]
  fsdr-plugin build --sdk <dir> <plugin crate dir> [--target-dir <dir>]
  fsdr-plugin info  --sdk <dir>";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        bail!("{USAGE}");
    };
    let mut out = None;
    let mut sdk = None;
    let mut workspace = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml"));
    let mut target_dir = None;
    let mut profile = Profile::Dev;
    let mut positional = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow::anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--sdk" => sdk = Some(PathBuf::from(value()?)),
            "--workspace" => workspace = PathBuf::from(value()?),
            "--target-dir" => target_dir = Some(PathBuf::from(value()?)),
            "--release" => profile = Profile::Release,
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
            let Some(out) = out else { bail!("pack needs --out\n{USAGE}") };
            let sdk = Sdk::pack(&workspace, profile, &out)?;
            println!("SDK in {} ({})", out.display(), sdk.rustc);
        }
        "build" => {
            let (Some(dir), [crate_dir]) = (sdk, positional.as_slice()) else {
                bail!("build needs --sdk and one plugin crate\n{USAGE}");
            };
            let sdk = Sdk::open(&dir)?;
            let target_dir = target_dir.unwrap_or_else(|| crate_dir.join("target"));
            let library = sdk.build_plugin(&crate_dir.join("Cargo.toml"), &target_dir)?;
            println!("{}", library.display());
        }
        "info" => {
            let Some(dir) = sdk else { bail!("info needs --sdk\n{USAGE}") };
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
