//! Plugins built against a packed SDK, and against the wrong build.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use futuresdr::blocks::VectorSink;
use futuresdr::prelude::*;
use plugin_host::Registry;
use plugin_host::Settings;
use plugin_sdk::Sdk;

fn workspace() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

fn basic_crate() -> std::path::PathBuf {
    workspace().join("blocks/basic/Cargo.toml")
}

#[test]
fn a_packed_sdk_builds_loadable_plugins() -> anyhow::Result<()> {
    let dir = common::scratch("packed");
    let packed = Sdk::of_this_process()?.pack(&dir.join("sdk"))?;

    let sdk = Sdk::open(&dir.join("sdk"))?;
    assert_eq!(sdk.rt, packed.rt);
    let files = std::fs::read_dir(&sdk.deps[0])?.count();
    assert!(
        files < 400,
        "the SDK holds only what the library needs: {files} files"
    );

    let library = sdk.build_plugin(&basic_crate(), &dir.join("target"))?;
    assert!(library.starts_with(&dir));

    let mut registry = Registry::new();
    registry.load(&library)?;
    let mut fg = Flowgraph::new();
    let values = HashMap::from([(
        "items".to_string(),
        Pmt::VecPmt(vec![Pmt::Isize(4), Pmt::Isize(5)]),
    )]);
    let src = registry.add(&mut fg, "VectorSource<i32>", &Settings::new("src", values))?;
    let snk = registry.add(&mut fg, "VectorSink<i32>", &Settings::default())?;
    fg.stream_dyn(src.id, "output", snk.id, "input")?;
    let done = Runtime::new().run(fg)?;
    let snk = *snk
        .block_ref
        .downcast::<BlockRef<VectorSink<i32>>>()
        .unwrap();
    assert_eq!(done.block(&snk)?.items(), &vec![4, 5]);
    Ok(())
}

/// A second build of the shared library, from the same sources but with
/// another profile setting: every symbol hash differs.
#[test]
fn a_plugin_built_against_another_build_is_refused() -> anyhow::Result<()> {
    let dir = common::scratch("other-build");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-q", "-p", "futuresdr-plugin-rt"])
        .arg("--manifest-path")
        .arg(workspace().join("Cargo.toml"))
        .arg("--target-dir")
        .arg(dir.join("target"))
        .args(["--config", "profile.dev.debug-assertions=false"])
        .status()?;
    assert!(status.success());

    let other = Sdk::of_library(&dir.join("target/debug/libfuturesdr_plugin_rt.so"))?;
    let ours = Sdk::of_this_process()?;
    let futuresdr = |sdk: &Sdk| -> anyhow::Result<String> {
        Ok(sdk
            .crates()?
            .into_iter()
            .find(|c| c.starts_with("futuresdr-"))
            .unwrap())
    };
    assert_ne!(
        futuresdr(&other)?,
        futuresdr(&ours)?,
        "the builds must differ"
    );

    let library = other.build_plugin(&basic_crate(), &dir.join("plugin"))?;
    let err = Registry::new().load(&library).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("was it built against this program's SDK") && msg.contains("undefined symbol"),
        "{msg}"
    );
    Ok(())
}
