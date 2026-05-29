//! Host-side machinery for loading and orchestrating FutureSDR plugins.
//!
//! This crate is linked into the *host* application, not into plugins. It
//! contains the dlopen-based [`LoadedPlugin`] loader, the [`FlowgraphController`]
//! and [`RadioController`], and TOML config parsing. Keeping it separate from
//! [`plugin_api`] (the tiny plugin-side ABI) is what keeps plugin `.so` files
//! small — a plugin embeds only `plugin_api`, never this crate.

mod bridge;
pub mod config_value;
pub mod flowgraph_controller;
pub mod radio_controller;

use std::any::Any;
use std::path::PathBuf;
use futuresdr::runtime::{Block, BlockId};
use futuresdr::runtime::abi_fingerprint;
use libloading::{Library, Symbol};

pub use plugin_api::{AbiFingerPrintFn, BlockFactory, CreateBlockFactoryFn};
pub use config_value::{ConfigValue, FromConfigValue, parse_typed_config};
pub use flowgraph_controller::{FlowgraphController, PluginRegistry};
pub use radio_controller::{RadioController, RadioOutputBuf};

/// Returns the default directory where plugin `.so` files are located.
///
/// Resolution order:
/// 1. `PLUGIN_DIR` environment variable (if set)
/// 2. The directory containing the running executable (works for both
///    `target/debug/` and deployed installs where plugins sit next to the binary)
/// 3. Falls back to `"."` if the exe path cannot be determined.
pub fn default_plugin_dir() -> String {
    if let Ok(dir) = std::env::var("PLUGIN_DIR") {
        return dir;
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_else(|| ".".to_string())
}

/// Builds the full path for a plugin `.so` given a directory and crate name.
///
/// Example: `plugin_path(&dir, "throttle_plugin")` → `"{dir}/libthrottle_plugin.so"`
pub fn plugin_path(dir: &str, crate_name: &str) -> PathBuf {
    PathBuf::from(dir).join(format!("lib{crate_name}.so"))
}

/// Error returned when a plugin's ABI fingerprint does not match the runtime.
#[derive(Debug)]
pub struct AbiMismatchError {
    pub plugin_path: String,
    pub plugin_fingerprint: String,
    pub plugin_detail: String,
    pub runtime_fingerprint: String,
    pub runtime_detail: String,
}

impl std::fmt::Display for AbiMismatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ABI mismatch loading plugin `{}`:\n  plugin: {} ({})\n  runtime: {} ({})",
            self.plugin_path,
            self.plugin_fingerprint, self.plugin_detail,
            self.runtime_fingerprint, self.runtime_detail,
        )
    }
}

impl std::error::Error for AbiMismatchError {}

pub struct LoadedPlugin {
    factory: Box<dyn BlockFactory>,
    _lib: Library,
}

impl LoadedPlugin {
    /// Load a plugin with ABI fingerprint validation.
    ///
    /// Panics if the plugin cannot be loaded or if its ABI fingerprint
    /// does not match the runtime's fingerprint.
    pub unsafe fn load(path: &str) -> Self {
        match unsafe { Self::try_load(path) } {
            Ok(plugin) => plugin,
            Err(e) => panic!("{}", e),
        }
    }

    /// Load a plugin with ABI fingerprint validation, returning an error
    /// on mismatch instead of panicking.
    pub unsafe fn try_load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let lib = unsafe {
            Library::new(path)
                .map_err(|e| format!("failed to load plugin `{}`: {}", path, e))?
        };

        // Validate ABI fingerprint if the symbol exists
        let fingerprint_result: Result<Symbol<AbiFingerPrintFn>, _> =
            unsafe { lib.get(b"plugin_abi_fingerprint") };

        if let Ok(fingerprint_fn) = fingerprint_result {
            let (plugin_fp, plugin_detail) = fingerprint_fn();
            let runtime_fp = abi_fingerprint::ABI_FINGERPRINT;
            let runtime_detail = abi_fingerprint::ABI_FINGERPRINT_DETAIL;

            if plugin_fp != runtime_fp {
                return Err(Box::new(AbiMismatchError {
                    plugin_path: path.to_string(),
                    plugin_fingerprint: plugin_fp.to_string(),
                    plugin_detail: plugin_detail.to_string(),
                    runtime_fingerprint: runtime_fp.to_string(),
                    runtime_detail: runtime_detail.to_string(),
                }));
            }
        }

        let func: Symbol<CreateBlockFactoryFn> = unsafe {
            lib.get(b"create_block_factory")
                .map_err(|e| format!("symbol `create_block_factory` not found in `{}`: {}", path, e))?
        };
        let factory = func();
        Ok(Self { factory, _lib: lib })
    }

    /// Load a plugin WITHOUT ABI fingerprint validation.
    ///
    /// Use this only when you know the plugin is compatible (e.g., during
    /// development or when the plugin was built from the same workspace).
    pub unsafe fn load_unchecked(path: &str) -> Self {
        let lib = unsafe {
            Library::new(path)
                .unwrap_or_else(|e| panic!("failed to load plugin `{}`: {}", path, e))
        };
        let func: Symbol<CreateBlockFactoryFn> = unsafe {
            lib.get(b"create_block_factory")
                .unwrap_or_else(|e| panic!("symbol `create_block_factory` not found in `{}`: {}", path, e))
        };
        let factory = func();
        Self { factory, _lib: lib }
    }

    pub fn block_name(&self) -> &'static str { self.factory.block_name() }
    pub fn block_description(&self) -> &'static str { self.factory.block_description() }

    /// Returns a closure compatible with fg.add_block_dyn()
    /// config is consumed into the closure
    pub fn prepare(&self, config: Box<dyn Any + Send>)
        -> impl FnOnce(BlockId) -> Box<dyn Block> + '_
    {
        move |id| self.factory.create_block(id, config)
    }
}
