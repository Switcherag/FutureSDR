//! Plugins built against a packed SDK, and against the wrong build.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use futuresdr::blocks::VectorSink;
use futuresdr::prelude::*;
use plugin_host::Registry;
use plugin_host::Settings;
use plugin_sdk::Profile;
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
    // Opened SDKs have absolute paths, which plugin builds need.
    assert_eq!(sdk.rt, packed.rt.canonicalize()?);
    let id = Sdk::of_this_process()?.build_id();
    assert_eq!(sdk.build_id(), id, "the same build, packed");
    Sdk::of_this_process()?.pack(&dir.join("sdk"))?;
    assert_eq!(
        Sdk::open(&dir.join("sdk"))?.build_id(),
        id,
        "and packed again"
    );
    assert_eq!(
        sdk.crates()?,
        Sdk::of_this_process()?.crates()?,
        "the packed SDK has the same crates, with the same hashes"
    );
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

const TINY: &str = r#"
extern crate futuresdr_plugin_rt as futuresdr;

use futuresdr::prelude::*;

export_plugin! {
    name: "tiny",
    blocks: [
        {
            name: "Tiny",
            types: [u8],
            description: "Copy.",
            add: |_s| blocks::Copy::<T>::new(),
        },
    ]
}
"#;

/// A plugin crate named `name` in `dir`, with `profile` appended to its
/// manifest.
fn tiny_plugin(dir: &Path, name: &str, profile: &str) -> std::path::PathBuf {
    let dir = dir.join(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
             [lib]\ncrate-type = [\"dylib\"]\n\n[workspace]\n\n{profile}"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), TINY).unwrap();
    dir.join("Cargo.toml")
}

/// The tests of a plugin crate run against the SDK, with arguments for
/// the test harness; failing tests fail the command.
#[test]
fn plugin_tests_run_against_the_sdk() -> anyhow::Result<()> {
    let dir = common::scratch("plugin-tests");
    let manifest = tiny_plugin(&dir, "tiny_tested", "");
    let tests = r#"
#[cfg(test)]
mod tests {
    use futuresdr::prelude::*;

    #[test]
    fn copies() -> Result<()> {
        let mut fg = Flowgraph::new();
        let src = fg.add(blocks::VectorSource::<u8>::new(vec![1, 2]))?;
        let copy = fg.add(blocks::Copy::<u8>::new())?;
        let snk = fg.add(blocks::VectorSink::<u8>::new(4))?;
        fg.stream_dyn(src.id(), "output", copy.id(), "input")?;
        fg.stream_dyn(copy.id(), "output", snk.id(), "input")?;
        let done = Runtime::new().run(fg)?;
        assert_eq!(done.block(&snk)?.items(), &vec![1, 2]);
        Ok(())
    }

    #[test]
    #[ignore]
    fn fails() {
        panic!("as asked");
    }
}
"#;
    let lib = manifest.with_file_name("src/lib.rs");
    std::fs::write(&lib, format!("{TINY}{tests}"))?;
    let sdk = Sdk::of_this_process()?;
    let target = dir.join("target");
    let args = |args: &[&str]| -> Vec<String> { args.iter().map(|a| a.to_string()).collect() };
    sdk.test_plugin(&manifest, &target, &args(&["--test-threads=1"]))?;
    let failed = sdk.test_plugin(&manifest, &target, &args(&["--include-ignored"]));
    let err = format!("{:#}", failed.unwrap_err());
    assert!(err.contains("testing plugin"), "{err}");
    Ok(())
}

/// Whether the ELF file has a symbol table (the dynamic one aside).
fn has_symtab(path: &Path) -> bool {
    const SHT_SYMTAB: u32 = 2;
    let elf = std::fs::read(path).unwrap();
    assert_eq!(&elf[..4], b"\x7fELF");
    let u16_at = |at: usize| u16::from_le_bytes(elf[at..at + 2].try_into().unwrap()) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap());
    let shoff = u64::from_le_bytes(elf[0x28..0x30].try_into().unwrap()) as usize;
    let (entry, count) = (u16_at(0x3a), u16_at(0x3c));
    (0..count).any(|i| u32_at(shoff + i * entry + 4) == SHT_SYMTAB)
}

/// Release plugins are built small unless their manifest says otherwise;
/// debug plugins as the manifest says.
#[test]
fn plugin_profile_defaults() -> anyhow::Result<()> {
    let dir = common::scratch("profile");
    let sdk = Sdk::of_this_process()?;
    let plain = sdk.build_plugin(&tiny_plugin(&dir, "tiny_plain", ""), &dir.join("target"))?;
    let kept = sdk.build_plugin(
        &tiny_plugin(&dir, "tiny_kept", "[profile.release]\nstrip = false\n"),
        &dir.join("target"),
    )?;
    let release = sdk.profile == Profile::Release;
    assert_eq!(has_symtab(&plain), !release, "{}", plain.display());
    assert!(has_symtab(&kept), "{}", kept.display());

    let mut registry = Registry::new();
    registry.load(&plain)?;
    assert!(registry.get("Tiny<u8>").is_some());
    Ok(())
}
