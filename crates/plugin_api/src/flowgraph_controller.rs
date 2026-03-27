//! Flowgraph controller — build and hot-swap flowgraphs from TOML definitions.
//!
//! # Architecture
//!
//! ```text
//!   FG0 (permanent):
//!     Source → ... → Selector
//!                      → outputs[0]: NullSink   (parking drain)
//!                      → outputs[1]: BridgeSink (→ shared buffer)
//!
//!   FG1 (swappable, loaded from TOML):
//!     BridgeSource (← shared buffer) → ... → Sink
//! ```
//!
//! The controller manages:
//! - A [`PluginRegistry`] that caches loaded `.so` plugins
//! - TOML-based flowgraph construction with full type support via
//!   [`ConfigValue`](crate::config_value::ConfigValue)
//! - Hot-swap sequence with automatic error recovery
//!
//! # TOML format
//!
//! ```toml
//! [[blocks]]
//! id = "src"
//! plugin = "bridge_source_plugin"
//! bridge = true                       # receives the shared bridge buffer
//!
//! [[blocks]]
//! id = "throttle"
//! plugin = "throttle_plugin"
//! config = 1000000.0                  # TOML float → auto-detected as f64
//!
//! [[blocks]]
//! id = "decoder"
//! plugin = "zigbee_decoder_plugin"
//! config = 11                         # integer, but plugin expects u32
//! config_type = "u32"                 # explicit type override
//!
//! [[blocks]]
//! id = "resampler"
//! plugin = "fir_resampler_plugin"
//! config = [3, 5]                     # TOML array → tuple
//! config_type = "(usize, usize)"
//!
//! [[blocks]]
//! id = "file_src"
//! plugin = "file_source_plugin"
//! config = ["/tmp/data.bin", true]
//! config_type = "(String, bool)"
//!
//! [[blocks]]
//! id = "sink"
//! plugin = "print_sink_plugin"
//! config = "My Flow"                  # auto-detected as String
//!
//! [[connections]]
//! src = "src.output"                  # block_id.port_name
//! dst = "throttle.input"
//!
//! [[connections]]
//! src = "throttle.output"
//! dst = "sink.input"
//! ```

use crate::config_value::{ConfigValue, parse_typed_config};
use crate::{LoadedPlugin, plugin_path};
use futuresdr::runtime::{BlockId, Flowgraph, FlowgraphHandle, Pmt, RuntimeHandle};
use serde::Deserialize;
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

// ════════════════════════════════════════════════════════════════════
// TOML schema
// ════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct FlowgraphDef {
    blocks: Vec<BlockDef>,
    connections: Vec<ConnectionDef>,
}

#[derive(Deserialize)]
struct BlockDef {
    id: String,
    plugin: String,
    /// If true, this block receives the shared bridge buffer as its config.
    #[serde(default)]
    bridge: bool,
    /// Config value — see module docs for type mapping.
    config: Option<toml::Value>,
    /// Explicit Rust type name for the config (e.g. `"u32"`, `"(usize, usize)"`).
    /// When omitted, auto-detection maps: string→String, float→f64, int→u64, bool→bool.
    config_type: Option<String>,
}

#[derive(Deserialize)]
struct ConnectionDef {
    /// `"block_id.port_name"` e.g. `"src.output"` or `"sel.outputs[1]"`
    src: String,
    /// `"block_id.port_name"` e.g. `"sink.input"` or `"sel.inputs[0]"`
    dst: String,
}

// ════════════════════════════════════════════════════════════════════
// Plugin registry
// ════════════════════════════════════════════════════════════════════

/// Caches loaded `.so` plugins by crate name, loading on demand.
pub struct PluginRegistry {
    dir: String,
    plugins: HashMap<String, LoadedPlugin>,
}

impl PluginRegistry {
    pub fn new(dir: impl Into<String>) -> Self {
        Self { dir: dir.into(), plugins: HashMap::new() }
    }

    /// Load a plugin into the cache (no-op if already loaded).
    pub fn ensure_loaded(&mut self, name: &str) {
        if !self.plugins.contains_key(name) {
            let path = plugin_path(&self.dir, name);
            let plugin = unsafe { LoadedPlugin::load(path.to_str().unwrap()) };
            self.plugins.insert(name.to_string(), plugin);
        }
    }

    /// Get a reference to a loaded plugin. Panics if not loaded.
    pub fn get(&self, name: &str) -> &LoadedPlugin {
        self.plugins
            .get(name)
            .unwrap_or_else(|| panic!("plugin '{name}' not loaded — call ensure_loaded first"))
    }

    pub fn dir(&self) -> &str {
        &self.dir
    }
}

// ════════════════════════════════════════════════════════════════════
// FlowgraphController
// ════════════════════════════════════════════════════════════════════

/// Controls a swappable flowgraph (FG1) connected to a permanent flowgraph (FG0)
/// through a shared bridge buffer and a Selector block.
///
/// The controller handles:
/// - Plugin loading and caching
/// - TOML-based flowgraph construction with full type support
/// - Hot-swap: park selector → terminate FG1 → clear buffer → build new FG1 →
///   start → unpark selector
/// - Error recovery: restores the previous flowgraph on swap failure
pub struct FlowgraphController {
    pub registry: PluginRegistry,
    shared_buf: Arc<Mutex<VecDeque<u8>>>,
    selector_id: BlockId,
    current_toml: String,
    custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
}

impl FlowgraphController {
    /// Create a new controller.
    ///
    /// - `plugin_dir`: directory containing `.so` plugin files
    /// - `selector_id`: the Selector block in FG0 that routes to the bridge
    pub fn new(plugin_dir: impl Into<String>, selector_id: BlockId) -> Self {
        Self {
            registry: PluginRegistry::new(plugin_dir),
            shared_buf: Arc::new(Mutex::new(VecDeque::new())),
            selector_id,
            current_toml: String::new(),
            custom_parsers: HashMap::new(),
        }
    }

    /// Register a custom config parser for a type name not covered by the built-in set.
    ///
    /// ```ignore
    /// ctrl.register_config_parser("SelectorDropPolicy", |v| {
    ///     let s = v.as_string()?;
    ///     match s.as_str() {
    ///         "DropAll" => Ok(Box::new(SelectorDropPolicy::DropAll)),
    ///         "DropNone" => Ok(Box::new(SelectorDropPolicy::DropNone)),
    ///         _ => Err(format!("unknown policy '{s}'")),
    ///     }
    /// });
    /// ```
    pub fn register_config_parser(
        &mut self,
        type_name: &str,
        parser: fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>,
    ) {
        self.custom_parsers.insert(type_name.to_string(), parser);
    }

    /// Returns a clone of the shared bridge buffer for use when building FG0.
    pub fn shared_buf(&self) -> Arc<Mutex<VecDeque<u8>>> {
        self.shared_buf.clone()
    }

    /// Returns the path of the currently active TOML flowgraph.
    pub fn current_toml(&self) -> &str {
        &self.current_toml
    }

    /// Build a `Flowgraph` from a TOML file.
    ///
    /// Automatically loads any plugins referenced in the TOML that aren't
    /// already cached. Config values are parsed according to `config_type`
    /// (or auto-detected if omitted).
    pub fn build_flowgraph(
        &mut self,
        toml_path: &str,
    ) -> Result<Flowgraph, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(toml_path)
            .map_err(|e| format!("cannot read '{toml_path}': {e}"))?;
        let def: FlowgraphDef = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML in '{toml_path}': {e}"))?;

        // Ensure all required plugins are loaded
        for block in &def.blocks {
            self.registry.ensure_loaded(&block.plugin);
        }

        let mut fg = Flowgraph::new();
        let mut block_ids: HashMap<String, BlockId> = HashMap::new();

        for block in &def.blocks {
            let config: Box<dyn Any + Send> = if block.bridge {
                Box::new(self.shared_buf.clone())
            } else {
                self.parse_block_config(block)?
            };

            let plugin = self.registry.get(&block.plugin);
            let id = fg.add_block_dyn(plugin.prepare(config));
            block_ids.insert(block.id.clone(), id);
        }

        for conn in &def.connections {
            let (src_block, src_port) = conn.src.split_once('.')
                .ok_or_else(|| format!("invalid src '{}', expected 'block.port'", conn.src))?;
            let (dst_block, dst_port) = conn.dst.split_once('.')
                .ok_or_else(|| format!("invalid dst '{}', expected 'block.port'", conn.dst))?;

            let &src_id = block_ids.get(src_block)
                .ok_or_else(|| format!("unknown block '{src_block}' in connection"))?;
            let &dst_id = block_ids.get(dst_block)
                .ok_or_else(|| format!("unknown block '{dst_block}' in connection"))?;

            fg.connect_dyn(src_id, src_port, dst_id, dst_port)
                .map_err(|e| format!("connect {src_block}.{src_port} → {dst_block}.{dst_port}: {e}"))?;
        }

        Ok(fg)
    }

    /// Start the initial swappable flowgraph from a TOML file.
    ///
    /// Routes the selector to output\[1\] (bridge) and starts the flowgraph.
    pub async fn start_initial(
        &mut self,
        toml_path: &str,
        fg0_handle: &mut FlowgraphHandle,
        rt_handle: &RuntimeHandle,
    ) -> Result<FlowgraphHandle, Box<dyn std::error::Error + Send + Sync>> {
        fg0_handle
            .callback(self.selector_id, "output_index", Pmt::U32(1))
            .await
            .map_err(|e| format!("failed to route selector: {e}"))?;

        let fg = self.build_flowgraph(toml_path)?;
        let handle = rt_handle.start(fg).await
            .map_err(|e| format!("failed to start flowgraph: {e}"))?;
        self.current_toml = toml_path.to_string();
        Ok(handle)
    }

    /// Hot-swap the running FG1 to a new flowgraph defined by a TOML file.
    ///
    /// On error, the previous flowgraph is automatically restored.
    /// Returns `Ok(())` on success, or `Err(message)` if the new TOML failed
    /// (but the previous flowgraph has been restored).
    pub async fn swap(
        &mut self,
        new_toml: &str,
        fg0_handle: &mut FlowgraphHandle,
        fg1_handle: &mut FlowgraphHandle,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // 1. Park selector → output[0] (NullSink)
        fg0_handle
            .callback(self.selector_id, "output_index", Pmt::U32(0))
            .await
            .map_err(|e| format!("failed to park selector: {e}"))?;

        // 2. Terminate old FG1
        fg1_handle.terminate_and_wait().await
            .map_err(|e| format!("failed to terminate FG1: {e}"))?;

        // 3. Clear the shared buffer
        self.shared_buf.lock().unwrap().clear();

        // 4. Build & start new FG1
        match self.build_flowgraph(new_toml) {
            Ok(fg) => {
                *fg1_handle = rt_handle.start(fg).await
                    .map_err(|e| format!("failed to start new FG1: {e}"))?;
                self.current_toml = new_toml.to_string();

                // 5. Unpark selector → output[1] (bridge)
                fg0_handle
                    .callback(self.selector_id, "output_index", Pmt::U32(1))
                    .await
                    .map_err(|e| format!("failed to unpark selector: {e}"))?;

                Ok(())
            }
            Err(e) => {
                // Fallback: restore the previous flowgraph
                let prev = self.current_toml.clone();
                let fg = self.build_flowgraph(&prev)
                    .map_err(|e2| format!("failed to restore '{prev}': {e2}"))?;
                *fg1_handle = rt_handle.start(fg).await
                    .map_err(|e2| format!("failed to start restored FG1: {e2}"))?;

                fg0_handle
                    .callback(self.selector_id, "output_index", Pmt::U32(1))
                    .await
                    .map_err(|e2| format!("failed to unpark after restore: {e2}"))?;

                Err(format!("swap failed ({e}), restored '{prev}'").into())
            }
        }
    }

    /// Terminate both FG1 and FG0.
    pub async fn shutdown(
        &self,
        fg0_handle: &mut FlowgraphHandle,
        fg1_handle: &mut FlowgraphHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        fg1_handle.terminate_and_wait().await
            .map_err(|e| format!("failed to terminate FG1: {e}"))?;
        fg0_handle.terminate_and_wait().await
            .map_err(|e| format!("failed to terminate FG0: {e}"))?;
        Ok(())
    }

    // ── Private ──────────────────────────────────────────────────

    fn parse_block_config(
        &self,
        block: &BlockDef,
    ) -> Result<Box<dyn Any + Send>, Box<dyn std::error::Error + Send + Sync>> {
        let value = match &block.config {
            None => ConfigValue::Unit,
            Some(v) => ConfigValue::from_toml(v.clone()),
        };

        let type_hint = block.config_type.as_deref();

        // Try custom parsers first
        if let Some(type_name) = type_hint {
            if let Some(parser) = self.custom_parsers.get(type_name) {
                return parser(value).map_err(|e| {
                    format!("block '{}': custom parser for '{}' failed: {e}", block.id, type_name)
                        .into()
                });
            }
        }

        parse_typed_config(value, type_hint).map_err(|e| {
            format!("block '{}': {e}", block.id).into()
        })
    }
}
