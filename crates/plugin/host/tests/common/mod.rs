#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use plugin_host::Registry;
use plugin_sdk::Sdk;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The `basic` plugin, compiled against the shared library this test
/// process has loaded — in a separate Cargo invocation, as a third party
/// would.
pub fn basic_plugin() -> PathBuf {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let sdk = Sdk::of_this_process().expect("locating the SDK of this process");
            sdk.build_plugin(
                &workspace().join("blocks/basic/Cargo.toml"),
                &workspace().join("target/plugins"),
            )
            .expect("building the basic plugin")
        })
        .clone()
}

/// A registry holding the `basic` plugin.
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .load(&basic_plugin())
        .expect("loading the basic plugin");
    registry
}
