//! Multi-flowgraph controller with builder pattern, auto-injected bridges, and UDP control.
//!
//! # Architecture
//!
//! Flowgraphs are defined in TOML. Each flowgraph declares **ports** — named
//! entry/exit points. The controller automatically injects bridge blocks
//! (sink/source) to transfer data between flowgraphs. Users never write bridge
//! blocks in TOML.
//!
//! ```text
//!   FG 0 (permanent):
//!     SeifySource -> Selector
//!                      -> outputs[0]: NullSink
//!                      -> outputs[1]: [auto BridgeSink] ──> shared buffer
//!
//!   FG 1 (swappable):
//!     [auto BridgeSource] <── shared buffer -> ... -> Decoder
//! ```
//!
//! # Builder usage
//!
//! ```ignore
//! FlowgraphController::builder(default_plugin_dir())
//!     .add_permanent("flows/fg0.toml")        // fg 0
//!     .add_swappable("flows/flow_a.toml")     // fg 1
//!     .connect(0, "out", 1, "in")
//!     .udp_port(7878)
//!     .run()?;
//! ```
//!
//! # TOML format
//!
//! ```toml
//! # Output port: controller auto-adds a bridge sink after sel.outputs[1]
//! [[ports]]
//! id = "out"
//! direction = "out"
//! src = "sel.outputs[1]"   # block.port to tap
//! router = "sel"            # optional: selector to park during swap
//! type = "c32"              # stream type: u8, f32, c32
//!
//! # Input port: controller auto-adds a bridge source before demod.input
//! [[ports]]
//! id = "in"
//! direction = "in"
//! dst = "demod.input"      # block.port to feed
//! type = "c32"
//! ```

use crate::bridge;
use crate::config_value::{ConfigValue, parse_typed_config};
use crate::{LoadedPlugin, plugin_path};
use futuresdr::runtime::{BlockId, Flowgraph, FlowgraphHandle, Pmt, Runtime, RuntimeHandle, WrappedKernel};
use serde::Deserialize;
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

// ════════════════════════════════════════════════════════════════════
// TOML schema
// ════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct FlowgraphDef {
    #[serde(default)]
    ports: Vec<PortDef>,
    #[serde(default)]
    blocks: Vec<BlockDef>,
    #[serde(default)]
    connections: Vec<ConnectionDef>,
}

#[derive(Deserialize, Clone)]
struct PortDef {
    id: String,
    direction: PortDirection,
    /// For output ports: `"block.port"` whose output gets bridged out.
    src: Option<String>,
    /// For input ports: `"block.port"` that receives bridged-in data.
    dst: Option<String>,
    /// Optional selector block to park/unpark during swap.
    router: Option<String>,
    /// Stream element type: `"u8"` (default), `"f32"`, `"c32"` (Complex32).
    #[serde(rename = "type", default)]
    stream_type: StreamType,
}

#[derive(Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
enum PortDirection {
    In,
    Out,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
enum StreamType {
    #[default]
    U8,
    F32,
    C32,
}

#[derive(Deserialize)]
struct BlockDef {
    id: String,
    plugin: String,
    config: Option<toml::Value>,
    config_type: Option<String>,
}

#[derive(Deserialize)]
struct ConnectionDef {
    src: String,
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

    pub fn ensure_loaded(&mut self, name: &str) {
        if !self.plugins.contains_key(name) {
            let path = plugin_path(&self.dir, name);
            let plugin = unsafe { LoadedPlugin::load(path.to_str().unwrap()) };
            self.plugins.insert(name.to_string(), plugin);
        }
    }

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
// Builder
// ════════════════════════════════════════════════════════════════════

struct FgEntry {
    toml_path: String,
    permanent: bool,
}

struct Connection {
    from_fg: usize,
    from_port: String,
    to_fg: usize,
    to_port: String,
}

/// Builder for [`FlowgraphController`].
pub struct FlowgraphControllerBuilder {
    plugin_dir: String,
    flowgraphs: Vec<FgEntry>,
    connections: Vec<Connection>,
    udp_port: Option<u16>,
    custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
}

impl FlowgraphControllerBuilder {
    /// Add a permanent (non-swappable) flowgraph.
    pub fn add_permanent(mut self, toml_path: impl Into<String>) -> Self {
        self.flowgraphs.push(FgEntry { toml_path: toml_path.into(), permanent: true });
        self
    }

    /// Add a swappable flowgraph (hot-swappable at runtime via UDP).
    pub fn add_swappable(mut self, toml_path: impl Into<String>) -> Self {
        self.flowgraphs.push(FgEntry { toml_path: toml_path.into(), permanent: false });
        self
    }

    /// Connect an output port of one flowgraph to an input port of another.
    ///
    /// Uses flowgraph indices (matching `FlowgraphId`) and port names from TOML.
    pub fn connect(
        mut self,
        from_fg: usize, from_port: impl Into<String>,
        to_fg: usize, to_port: impl Into<String>,
    ) -> Self {
        self.connections.push(Connection {
            from_fg, from_port: from_port.into(),
            to_fg, to_port: to_port.into(),
        });
        self
    }

    /// Set the UDP control port (default: 7878).
    pub fn udp_port(mut self, port: u16) -> Self {
        self.udp_port = Some(port);
        self
    }

    /// Register a custom config parser for a type name not covered by built-ins.
    pub fn register_config_parser(
        mut self,
        type_name: &str,
        parser: fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>,
    ) -> Self {
        self.custom_parsers.insert(type_name.to_string(), parser);
        self
    }

    /// Build, start, and run the full system. Blocks until shutdown.
    pub fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Build channel keys and shared buffers
        let mut channels: HashMap<String, Arc<Mutex<VecDeque<u8>>>> = HashMap::new();
        for conn in &self.connections {
            let from_key = format!("{}:{}", conn.from_fg, conn.from_port);
            let to_key = format!("{}:{}", conn.to_fg, conn.to_port);
            let buf = Arc::new(Mutex::new(VecDeque::new()));
            channels.insert(from_key, buf.clone());
            channels.insert(to_key, buf);
        }

        let udp_port = self.udp_port.unwrap_or(7878);
        let socket = std::net::UdpSocket::bind(format!("0.0.0.0:{udp_port}"))
            .map_err(|e| format!("bind UDP :{udp_port}: {e}"))?;
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

        let mut registry = PluginRegistry::new(&self.plugin_dir);
        let custom_parsers = self.custom_parsers;

        // Pre-load all plugins
        for entry in &self.flowgraphs {
            let content = std::fs::read_to_string(&entry.toml_path)
                .map_err(|e| format!("cannot read '{}': {e}", entry.toml_path))?;
            let def: FlowgraphDef = toml::from_str(&content)
                .map_err(|e| format!("invalid TOML '{}': {e}", entry.toml_path))?;
            for block in &def.blocks {
                registry.ensure_loaded(&block.plugin);
            }
        }

        let rt = Runtime::new();
        let rt_handle = rt.handle();
        let flowgraphs = self.flowgraphs;
        let connections = self.connections;

        println!("Listening for UDP commands on port {udp_port}");
        println!("  -s <path.toml>              swap first swappable FG");
        println!("  -s <fg_index> <path.toml>   swap specific FG by index");
        println!("  Q                           quit\n");

        rt.block_on(async move {
            let mut ctrl = FlowgraphController::new(
                registry, channels, custom_parsers, connections,
            );

            // Start permanent FGs
            for (idx, entry) in flowgraphs.iter().enumerate() {
                if entry.permanent {
                    println!("Starting permanent flowgraph {idx} from '{}' ...", entry.toml_path);
                    ctrl.start_permanent(idx, &entry.toml_path, &rt_handle).await?;
                    println!("  fg/{idx}/ running.");
                }
            }

            ctrl.activate_selectors().await?;

            // Start swappable FGs
            for (idx, entry) in flowgraphs.iter().enumerate() {
                if !entry.permanent {
                    println!("Starting swappable flowgraph {idx} from '{}' ...", entry.toml_path);
                    ctrl.start_swappable(idx, &entry.toml_path, &rt_handle).await?;
                    println!("  fg/{idx}/ running.");
                }
            }

            println!("\nAll flowgraphs running.\n");

            let first_swappable_idx = flowgraphs.iter()
                .enumerate()
                .find(|(_, e)| !e.permanent)
                .map(|(i, _)| i);

            let mut udp_buf = [0u8; 512];
            loop {
                futuresdr::async_io::Timer::after(std::time::Duration::from_millis(100)).await;

                match socket.recv_from(&mut udp_buf) {
                    Ok((n, addr)) => {
                        let cmd = std::str::from_utf8(&udp_buf[..n]).unwrap_or("").trim();
                        println!("UDP from {addr}: \"{cmd}\"");

                        if cmd.eq_ignore_ascii_case("q") {
                            println!("Shutting down ...");
                            ctrl.shutdown_all().await;
                            break;
                        } else if let Some(rest) = cmd.strip_prefix("-s ").or_else(|| cmd.strip_prefix("-S ")) {
                            let rest = rest.trim();
                            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                            let (fg_idx, toml_path) = if parts.len() == 2 {
                                if let Ok(idx) = parts[0].parse::<usize>() {
                                    (idx, parts[1].to_string())
                                } else {
                                    eprintln!("ERROR: invalid fg index '{}'", parts[0]);
                                    continue;
                                }
                            } else if let Some(idx) = first_swappable_idx {
                                (idx, parts[0].to_string())
                            } else {
                                eprintln!("ERROR: no swappable flowgraph defined");
                                continue;
                            };

                            println!("Swapping fg/{fg_idx}/ to '{toml_path}' ...");
                            match ctrl.swap(fg_idx, &toml_path, &rt_handle).await {
                                Ok(()) => println!("fg/{fg_idx}/ now running '{toml_path}'\n"),
                                Err(e) => eprintln!("ERROR: {e}\n"),
                            }
                        } else {
                            println!("Unknown command. Use: -s [fg_index] <path.toml> | Q");
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => eprintln!("UDP error: {e}"),
                }
            }

            println!("Done.");
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
    }
}

// ════════════════════════════════════════════════════════════════════
// FlowgraphController
// ════════════════════════════════════════════════════════════════════

struct SelectorInfo {
    perm_fg: usize,
    block_id: BlockId,
    channel_key: String,
}

struct SwappableState {
    handle: FlowgraphHandle,
    current_toml: String,
}

/// Controls multiple flowgraphs connected through auto-injected bridge blocks.
pub struct FlowgraphController {
    registry: PluginRegistry,
    channels: HashMap<String, Arc<Mutex<VecDeque<u8>>>>,
    custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
    connections: Vec<Connection>,
    perm_handles: HashMap<usize, FlowgraphHandle>,
    selector_infos: Vec<SelectorInfo>,
    swap_states: HashMap<usize, SwappableState>,
}

impl FlowgraphController {
    /// Create a builder for configuring the controller.
    pub fn builder(plugin_dir: impl Into<String>) -> FlowgraphControllerBuilder {
        FlowgraphControllerBuilder {
            plugin_dir: plugin_dir.into(),
            flowgraphs: Vec::new(),
            connections: Vec::new(),
            udp_port: None,
            custom_parsers: HashMap::new(),
        }
    }

    fn new(
        registry: PluginRegistry,
        channels: HashMap<String, Arc<Mutex<VecDeque<u8>>>>,
        custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
        connections: Vec<Connection>,
    ) -> Self {
        Self {
            registry, channels, custom_parsers, connections,
            perm_handles: HashMap::new(),
            selector_infos: Vec::new(),
            swap_states: HashMap::new(),
        }
    }

    async fn start_permanent(
        &mut self,
        fg_idx: usize,
        toml_path: &str,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (fg, block_ids, port_defs) = self.build_flowgraph_full(fg_idx, toml_path)?;
        let handle = rt_handle.start(fg).await
            .map_err(|e| format!("start permanent fg/{fg_idx}/: {e}"))?;

        for port in &port_defs {
            if port.direction == PortDirection::Out {
                if let Some(ref router_id) = port.router {
                    let sel_block_id = *block_ids.get(router_id)
                        .ok_or_else(|| format!(
                            "router '{}' not found in fg/{fg_idx}/", router_id
                        ))?;
                    self.selector_infos.push(SelectorInfo {
                        perm_fg: fg_idx,
                        block_id: sel_block_id,
                        channel_key: format!("{fg_idx}:{}", port.id),
                    });
                }
            }
        }

        self.perm_handles.insert(fg_idx, handle);
        Ok(())
    }

    async fn activate_selectors(
        &mut self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for sel in &self.selector_infos {
            let handle = self.perm_handles.get_mut(&sel.perm_fg)
                .ok_or_else(|| format!("perm fg/{} not found", sel.perm_fg))?;
            handle.callback(sel.block_id, "output_index", Pmt::U32(1)).await
                .map_err(|e| format!("activate selector in fg/{}/: {e}", sel.perm_fg))?;
        }
        Ok(())
    }

    async fn start_swappable(
        &mut self,
        fg_idx: usize,
        toml_path: &str,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (fg, _, _) = self.build_flowgraph_full(fg_idx, toml_path)?;
        let handle = rt_handle.start(fg).await
            .map_err(|e| format!("start swappable fg/{fg_idx}/: {e}"))?;
        self.swap_states.insert(fg_idx, SwappableState {
            handle,
            current_toml: toml_path.to_string(),
        });
        Ok(())
    }

    async fn swap(
        &mut self,
        fg_idx: usize,
        new_toml: &str,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let state = self.swap_states.get(&fg_idx)
            .ok_or_else(|| format!("unknown swappable fg/{fg_idx}/"))?;
        let prev_toml = state.current_toml.clone();

        // 1. Park selectors connected to this FG (if any)
        let connected_sels: Vec<usize> = self.selector_infos.iter()
            .enumerate()
            .filter(|(_, sel)| {
                self.connections.iter().any(|conn| {
                    let from_key = format!("{}:{}", conn.from_fg, conn.from_port);
                    from_key == sel.channel_key && conn.to_fg == fg_idx
                })
            })
            .map(|(i, _)| i)
            .collect();

        for &idx in &connected_sels {
            let sel = &self.selector_infos[idx];
            let handle = self.perm_handles.get_mut(&sel.perm_fg).unwrap();
            handle.callback(sel.block_id, "output_index", Pmt::U32(0)).await
                .map_err(|e| format!("park selector: {e}"))?;
        }

        // 2. Terminate old FG
        self.swap_states.get_mut(&fg_idx).unwrap()
            .handle.terminate_and_wait().await
            .map_err(|e| format!("terminate fg/{fg_idx}/: {e}"))?;

        // 3. Clear connected buffers
        for conn in &self.connections {
            if conn.to_fg == fg_idx {
                let from_key = format!("{}:{}", conn.from_fg, conn.from_port);
                if let Some(buf) = self.channels.get(&from_key) {
                    buf.lock().unwrap().clear();
                }
            }
        }

        // 4. Load new plugins if needed
        let content = std::fs::read_to_string(new_toml)
            .map_err(|e| format!("cannot read '{new_toml}': {e}"))?;
        let def: FlowgraphDef = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML '{new_toml}': {e}"))?;
        for block in &def.blocks {
            self.registry.ensure_loaded(&block.plugin);
        }

        // 5. Build & start (with fallback)
        match self.build_flowgraph_full(fg_idx, new_toml) {
            Ok((fg, _, _)) => {
                match rt_handle.start(fg).await {
                    Ok(handle) => {
                        let state = self.swap_states.get_mut(&fg_idx).unwrap();
                        state.handle = handle;
                        state.current_toml = new_toml.to_string();
                    }
                    Err(e) => {
                        self.restore_and_unpark(fg_idx, &prev_toml, &connected_sels, rt_handle).await?;
                        return Err(format!("start fg/{fg_idx}/ failed ({e}), restored '{prev_toml}'").into());
                    }
                }
            }
            Err(e) => {
                self.restore_and_unpark(fg_idx, &prev_toml, &connected_sels, rt_handle).await?;
                return Err(format!("build fg/{fg_idx}/ failed ({e}), restored '{prev_toml}'").into());
            }
        }

        // 6. Unpark selectors
        for &idx in &connected_sels {
            let sel = &self.selector_infos[idx];
            let handle = self.perm_handles.get_mut(&sel.perm_fg).unwrap();
            handle.callback(sel.block_id, "output_index", Pmt::U32(1)).await
                .map_err(|e| format!("unpark selector: {e}"))?;
        }

        Ok(())
    }

    async fn restore_and_unpark(
        &mut self,
        fg_idx: usize,
        prev_toml: &str,
        connected_sels: &[usize],
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (fg, _, _) = self.build_flowgraph_full(fg_idx, prev_toml)
            .map_err(|e| format!("failed to restore '{prev_toml}': {e}"))?;
        let handle = rt_handle.start(fg).await
            .map_err(|e| format!("failed to start restored fg/{fg_idx}/: {e}"))?;
        self.swap_states.get_mut(&fg_idx).unwrap().handle = handle;

        for &idx in connected_sels {
            let sel = &self.selector_infos[idx];
            let h = self.perm_handles.get_mut(&sel.perm_fg).unwrap();
            h.callback(sel.block_id, "output_index", Pmt::U32(1)).await
                .map_err(|e| format!("unpark after restore: {e}"))?;
        }
        Ok(())
    }

    async fn shutdown_all(&mut self) {
        for (idx, state) in &mut self.swap_states {
            if let Err(e) = state.handle.terminate_and_wait().await {
                eprintln!("warn: terminate swappable fg/{idx}/: {e}");
            }
        }
        for (idx, handle) in &mut self.perm_handles {
            if let Err(e) = handle.terminate_and_wait().await {
                eprintln!("warn: terminate permanent fg/{idx}/: {e}");
            }
        }
    }

    // ── Flowgraph building with auto-injected bridges ──────────────

    fn build_flowgraph_full(
        &self,
        fg_idx: usize,
        toml_path: &str,
    ) -> Result<(Flowgraph, HashMap<String, BlockId>, Vec<PortDef>), Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(toml_path)
            .map_err(|e| format!("cannot read '{toml_path}': {e}"))?;
        let def: FlowgraphDef = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML in '{toml_path}': {e}"))?;

        let mut fg = Flowgraph::new();
        let mut block_ids: HashMap<String, BlockId> = HashMap::new();

        // 1. Build all user-declared blocks
        for block in &def.blocks {
            let config = self.parse_block_config(block)?;
            let plugin = self.registry.get(&block.plugin);
            let id = fg.add_block_dyn(plugin.prepare(config));
            block_ids.insert(block.id.clone(), id);
        }

        // 2. Make all user-declared connections
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
                .map_err(|e| format!("connect {src_block}.{src_port} -> {dst_block}.{dst_port}: {e}"))?;
        }

        // 3. Auto-inject bridge blocks for each port
        for port in &def.ports {
            let channel_key = format!("{fg_idx}:{}", port.id);
            let buf = self.channels.get(&channel_key)
                .ok_or_else(|| format!(
                    "no channel for '{channel_key}' — check connect() calls"
                ))?;

            match port.direction {
                PortDirection::Out => {
                    // Auto-inject bridge sink: src_block.src_port -> bridge_sink.input
                    let src_spec = port.src.as_ref()
                        .ok_or_else(|| format!("output port '{}' missing 'src' field", port.id))?;
                    let (src_block, src_port) = src_spec.split_once('.')
                        .ok_or_else(|| format!("invalid port src '{src_spec}', expected 'block.port'"))?;
                    let &src_id = block_ids.get(src_block)
                        .ok_or_else(|| format!("unknown block '{src_block}' in port '{}'", port.id))?;

                    let bridge_id = add_bridge_sink(&mut fg, &port.stream_type, buf.clone());
                    fg.connect_dyn(src_id, src_port, bridge_id, "input")
                        .map_err(|e| format!("connect {src_spec} -> bridge_sink: {e}"))?;
                }
                PortDirection::In => {
                    // Auto-inject bridge source: bridge_source.output -> dst_block.dst_port
                    let dst_spec = port.dst.as_ref()
                        .ok_or_else(|| format!("input port '{}' missing 'dst' field", port.id))?;
                    let (dst_block, dst_port) = dst_spec.split_once('.')
                        .ok_or_else(|| format!("invalid port dst '{dst_spec}', expected 'block.port'"))?;
                    let &dst_id = block_ids.get(dst_block)
                        .ok_or_else(|| format!("unknown block '{dst_block}' in port '{}'", port.id))?;

                    let bridge_id = add_bridge_source(&mut fg, &port.stream_type, buf.clone());
                    fg.connect_dyn(bridge_id, "output", dst_id, dst_port)
                        .map_err(|e| format!("connect bridge_source -> {dst_spec}: {e}"))?;
                }
            }
        }

        Ok((fg, block_ids, def.ports))
    }

    fn parse_block_config(
        &self,
        block: &BlockDef,
    ) -> Result<Box<dyn Any + Send>, Box<dyn std::error::Error + Send + Sync>> {
        let value = match &block.config {
            None => ConfigValue::Unit,
            Some(v) => ConfigValue::from_toml(v.clone()),
        };
        let type_hint = block.config_type.as_deref();

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

// ── Bridge block factories ─────────────────────────────────────────

fn add_bridge_sink(
    fg: &mut Flowgraph,
    stream_type: &StreamType,
    buf: Arc<Mutex<VecDeque<u8>>>,
) -> BlockId {
    match stream_type {
        StreamType::U8 => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSinkU8::new(buf), id))
        }),
        StreamType::F32 => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSinkF32::new(buf), id))
        }),
        StreamType::C32 => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSinkC32::new(buf), id))
        }),
    }
}

fn add_bridge_source(
    fg: &mut Flowgraph,
    stream_type: &StreamType,
    buf: Arc<Mutex<VecDeque<u8>>>,
) -> BlockId {
    match stream_type {
        StreamType::U8 => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSourceU8::new(buf), id))
        }),
        StreamType::F32 => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSourceF32::new(buf), id))
        }),
        StreamType::C32 => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSourceC32::new(buf), id))
        }),
    }
}
