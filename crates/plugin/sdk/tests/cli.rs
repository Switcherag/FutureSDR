//! The `fsdr-plugin` command line, and SDKs that are not what they should be.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use plugin_sdk::Sdk;

/// Run `fsdr-plugin`: (success, stdout, stderr).
fn fsdr_plugin(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fsdr-plugin"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("deps")).unwrap();
    dir
}

/// An SDK directory whose library does not exist.
fn fake_sdk(name: &str, manifest: &str) -> PathBuf {
    let dir = scratch(name);
    fs::write(dir.join("sdk.env"), manifest).unwrap();
    dir
}

const MANIFEST: &str =
    "rt=libfuturesdr_plugin_rt.so\nprofile=release\ntoolchain=\nrustc=rustc 1.0\n";

#[test]
fn usage_and_mistakes() {
    for help in [["--help"].as_slice(), &["-h"], &["info", "--help"]] {
        let (ok, out, err) = fsdr_plugin(help);
        assert!(ok && out.contains("usage:"), "{help:?}: {err}");
    }
    let missing = scratch("missing").join("nothing");
    let missing = missing.to_str().unwrap();
    for (args, expected) in [
        (vec![], "usage:"),
        (vec!["frobnicate"], "unknown command"),
        (vec!["pack"], "pack needs --from and --out"),
        (vec!["pack", "--from", missing, "--out", missing], "nothing"),
        (vec!["build", "--sdk", missing], "build needs"),
        (vec!["info"], "info needs --sdk"),
        (vec!["info", "--bogus"], "unknown option --bogus"),
        (vec!["info", "--sdk"], "--sdk needs a value"),
        (vec!["info", "--sdk", missing], "is not an SDK"),
    ] {
        let (ok, _, err) = fsdr_plugin(&args);
        assert!(!ok && err.contains(expected), "{args:?}: {err}");
    }
}

#[test]
fn info_describes_an_sdk() {
    let dir = fake_sdk("info", MANIFEST);
    let (ok, out, err) = fsdr_plugin(&["info", "--sdk", dir.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert!(
        out.contains("profile   Release") && out.contains("rustc 1.0"),
        "{out}"
    );

    for (manifest, expected) in [
        (
            "rt=x\nprofile=fast\ntoolchain=\nrustc=\n",
            "unknown profile 'fast'",
        ),
        ("rt=x\n", "missing 'profile'"),
    ] {
        let dir = fake_sdk("broken", manifest);
        let err = format!("{:#}", Sdk::open(&dir).unwrap_err());
        assert!(err.contains(expected), "{err}");
    }
}

/// Builds against an SDK whose library does not exist fail, with and without
/// profile settings of the plugin's own, and with the lint options.
#[test]
fn a_plugin_that_does_not_build_is_reported() {
    for (name, profile) in [
        ("plain", ""),
        (
            "kept",
            "[profile.release]\nstrip = false\ncodegen-units = 16\n",
        ),
    ] {
        let sdk = fake_sdk(name, MANIFEST);
        let krate = sdk.join("plugin");
        fs::create_dir_all(krate.join("src")).unwrap();
        fs::write(
            krate.join("Cargo.toml"),
            format!(
                "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [lib]\ncrate-type = [\"dylib\"]\n[workspace]\n{profile}"
            ),
        )
        .unwrap();
        fs::write(
            krate.join("src/lib.rs"),
            "extern crate futuresdr_plugin_rt;\n",
        )
        .unwrap();
        let (ok, _, err) = fsdr_plugin(&[
            "build",
            "--sdk",
            sdk.to_str().unwrap(),
            krate.to_str().unwrap(),
            "--clippy",
            "--deny-warnings",
        ]);
        assert!(!ok && err.contains("building plugin"), "{err}");
    }
    let sdk = Sdk::open(&fake_sdk("manifest", MANIFEST)).unwrap();
    let err = format!(
        "{:#}",
        sdk.build_plugin(Path::new("/nonexistent/Cargo.toml"), Path::new("/tmp"))
            .unwrap_err()
    );
    assert!(err.contains("/nonexistent/Cargo.toml"), "{err}");
}

#[test]
fn only_libraries_in_target_directories_make_an_sdk() {
    let err = format!("{:#}", Sdk::of_this_process().unwrap_err());
    assert!(err.contains("has not loaded"), "{err}");
    let dir = scratch("outside");
    let lib = dir.join("libfuturesdr_plugin_rt.so");
    fs::write(&lib, "").unwrap();
    let err = format!("{:#}", Sdk::of_library(&lib).unwrap_err());
    assert!(err.contains("not in a Cargo target directory"), "{err}");
    assert!(Sdk::of_library(&dir.join("missing.so")).is_err());
}
