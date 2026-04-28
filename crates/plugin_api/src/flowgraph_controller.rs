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
use futuresdr::futures::channel::mpsc;
use futuresdr::prelude::Complex32;
use futuresdr::runtime::{BlockId, Flowgraph, FlowgraphHandle, Pmt, Runtime, RuntimeHandle, WrappedKernel};
use serde::Deserialize;
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

// ════════════════════════════════════════════════════════════════════
// TOML schema
// ════════════════════════════════════════════════════════════════════


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
    /// Optional source marker for input ports. Set `from = "head"` to
    /// auto-wire this port to the registered radio head's output.
    #[serde(default)]
    from: Option<String>,
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
    U8,
    F32,
    #[default]
    C32,
}

/// A type-tagged shared buffer between a bridge sink and source.
///
/// The element type is encoded in the enum variant, so bridge blocks are
/// created with the correct typed `VecDeque<T>` — no serialisation to bytes.
#[derive(Clone)]
enum SharedBuf {
    U8(Arc<Mutex<VecDeque<u8>>>),
    F32(Arc<Mutex<VecDeque<f32>>>),
    C32(Arc<Mutex<VecDeque<Complex32>>>),
}

impl SharedBuf {
    fn new(t: &StreamType) -> Self {
        match t {
            StreamType::U8  => SharedBuf::U8(Arc::new(Mutex::new(VecDeque::new()))),
            StreamType::F32 => SharedBuf::F32(Arc::new(Mutex::new(VecDeque::new()))),
            StreamType::C32 => SharedBuf::C32(Arc::new(Mutex::new(VecDeque::new()))),
        }
    }

    fn clear(&self) {
        match self {
            SharedBuf::U8(b)  => b.lock().unwrap().clear(),
            SharedBuf::F32(b) => b.lock().unwrap().clear(),
            SharedBuf::C32(b) => b.lock().unwrap().clear(),
        }
    }
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
    /// If true, this is a message connection (not a stream connection).
    #[serde(default)]
    message: bool,
}

/// Declares a message output that the FlowgraphController taps into.
/// PMTs emitted by `src` are forwarded to the controller's policy closure
/// tagged with `name`.
#[derive(Deserialize, Clone)]
struct TapDef {
    /// `"block.port"` whose message output is tapped.
    src: String,
    /// Tap name used to identify the source in policy callbacks.
    name: String,
}

/// `[head]` section of a radio-head TOML — frontend metadata + a map
/// from each protocol-`[radio]` demand to the block that services it:
///
///   `frequency_hz`   → `sdr_block` via "freq" message
///   `gain_db`        → `sdr_block` via "gain" message
///   `sample_rate_hz` → `resampler_block` (or `sdr_block` if no resampler)
///
/// Concrete hardware specs (rates, device args) are read from the head's
/// `[[blocks]]` config arrays, not duplicated here.
#[derive(Deserialize, Debug, Clone, Default)]
#[allow(dead_code)] // exposed for frontend introspection; not all fields
                    // are consumed by the runtime today.
pub struct HeadSectionDef {
    pub name: Option<String>,
    pub direction: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Block id that responds to "freq" / "gain" messages.
    pub sdr_block: Option<String>,
    /// Block id whose ratio satisfies a `sample_rate_hz` demand.
    /// When absent, the head delivers a fixed rate and protocols must
    /// request exactly that rate.
    pub resampler_block: Option<String>,
    pub output_port: Option<String>,
}

#[derive(Deserialize)]
struct HeadFile {
    head: HeadSectionDef,
}

/// Optional `[radio]` section. Two roles:
///
/// 1. **Provisioning**: at startup, if no `RadioController` is attached, the
///    first flowgraph whose `[radio]` carries `frequency_hz`,
///    `sample_rate_hz`, and `gain_db` is used to lazily build one and wire
///    it to that flowgraph's first c32 input port. `hardware_rate_hz`
///    (default 20 MSps) and `device_args` (default `""`) are optional.
///
/// 2. **Retuning**: on `swap()`, the new TOML's `frequency_hz`/`gain_db` are
///    re-applied to the attached radio as live message callbacks.
///    `sample_rate_hz` and `hardware_rate_hz` are *not* applied at swap time
///    (the resampler chain is fixed once started); flows that need a different
///    protocol rate must prepend their own resampler.
#[derive(Deserialize, Default)]
struct RadioSectionDef {
    frequency_hz: Option<f64>,
    gain_db: Option<f64>,
    sample_rate_hz: Option<f64>,
    hardware_rate_hz: Option<f64>,
    device_args: Option<String>,
}

// Head TOMLs carry a `[head]` section. serde silently ignores it here
// (no `deny_unknown_fields`); it's parsed separately via `HeadFile`.
#[derive(Deserialize)]
struct FlowgraphDef {
    #[serde(default)]
    ports: Vec<PortDef>,
    #[serde(default)]
    blocks: Vec<BlockDef>,
    #[serde(default)]
    connections: Vec<ConnectionDef>,
    #[serde(default)]
    controller_taps: Vec<TapDef>,
    #[serde(default)]
    radio: Option<RadioSectionDef>,
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

    pub fn ensure_loaded(&mut self, name: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.plugins.contains_key(name) {
            let path = plugin_path(&self.dir, name);
            let plugin = unsafe {
                LoadedPlugin::try_load(path.to_str().unwrap())
                    .map_err(|e| format!("load plugin '{name}': {e}"))?
            };
            self.plugins.insert(name.to_string(), plugin);
        }
        Ok(())
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

/// An external C32 buffer feeding into a flowgraph input port.
struct RadioInput {
    buf: crate::radio_controller::RadioOutputBuf,
    to_fg: usize,
    to_port: String,
}

/// Builder for [`FlowgraphController`].
pub struct FlowgraphControllerBuilder {
    plugin_dir: String,
    flowgraphs: Vec<FgEntry>,
    connections: Vec<Connection>,
    radio_inputs: Vec<RadioInput>,
    udp_port: Option<u16>,
    custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
    radio: Option<crate::radio_controller::RadioController>,
    tap_channel: Option<(mpsc::Sender<(String, Pmt)>, usize)>,
    /// Optional radio-head TOML — see [`HeadSectionDef`].
    head_path: Option<String>,
    /// Index of the head in `flowgraphs` (set by `add_head`).
    head_fg_idx: Option<usize>,
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

    /// Register a radio-head TOML.
    ///
    /// The head is a real permanent flowgraph: its `[[blocks]]`,
    /// `[[connections]]`, `[[ports]]` are built by the same machinery as
    /// any other flowgraph. The `[head]` section declares which block
    /// receives `freq`/`gain` retune messages (`sdr_block`) and which
    /// port feeds protocols (`output_port`).
    ///
    /// On startup the head's `output_port` is auto-wired to the first
    /// c32 input port of the first swappable flowgraph; on every swap,
    /// the protocol's `[radio]` `frequency_hz`/`gain_db` are dispatched
    /// to the head's `sdr_block` via live message callbacks.
    pub fn add_head(mut self, toml_path: impl Into<String>) -> Self {
        let path = toml_path.into();
        let idx = self.flowgraphs.len();
        self.flowgraphs.push(FgEntry {
            toml_path: path.clone(),
            permanent: true,
        });
        self.head_path = Some(path);
        self.head_fg_idx = Some(idx);
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

    /// Connect a [`RadioController`](crate::RadioController) output buffer to a flowgraph input port.
    ///
    /// The RadioController's BridgeSink writes to the buffer; the FlowgraphController
    /// auto-injects a BridgeSource on the receiving end.
    pub fn connect_radio(
        mut self,
        radio_buf: crate::radio_controller::RadioOutputBuf,
        to_fg: usize,
        to_port: impl Into<String>,
    ) -> Self {
        self.radio_inputs.push(RadioInput {
            buf: radio_buf,
            to_fg,
            to_port: to_port.into(),
        });
        self
    }

    /// Attach a [`RadioController`](crate::RadioController) to the flowgraph controller.
    ///
    /// This is a superset of [`connect_radio`]: it moves the radio into the
    /// controller (so swap() can auto-retune it via `[radio]` sections in
    /// swap-target TOMLs) AND wires its output buffer to a flowgraph input
    /// port. The radio is NOT started automatically — call
    /// [`FlowgraphController::start_radio`] from your `run_with` closure.
    pub fn attach_radio(
        mut self,
        radio: crate::radio_controller::RadioController,
        to_fg: usize,
        to_port: impl Into<String>,
    ) -> Self {
        let buf = radio.output_buf().clone();
        self.radio = Some(radio);
        self.radio_inputs.push(RadioInput {
            buf,
            to_fg,
            to_port: to_port.into(),
        });
        self
    }

    /// Create an mpsc channel for `[[controller_taps]]` PMTs.
    ///
    /// Every PMT emitted by a tapped port is forwarded to this channel as
    /// `(tap_name, pmt)`. Use a reasonably-sized buffer — at least a few
    /// tens of messages — so that bursty MAC output does not block the
    /// flowgraph.
    pub fn tap_channel(mut self, buffer: usize) -> (Self, mpsc::Receiver<(String, Pmt)>) {
        let (tx, rx) = mpsc::channel(buffer);
        self.tap_channel = Some((tx, buffer));
        (self, rx)
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

    /// Build, start all flowgraphs, and hand control to a user-supplied async closure.
    ///
    /// The closure receives `(&mut FlowgraphController, &RuntimeHandle, &[(usize, String, bool)])`.
    /// Use this for benchmarks or programmatic control instead of the UDP loop.
    pub fn run_with<F, Fut>(
        mut self,
        f: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce(FlowgraphController, RuntimeHandle, Vec<(usize, String, bool)>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'static,
    {
        let mut port_types: HashMap<(usize, String), StreamType> = HashMap::new();
        // Cached parsed defs (to avoid re-reading TOMLs for radio auto-provision below).
        let mut parsed_defs: Vec<FlowgraphDef> = Vec::with_capacity(self.flowgraphs.len());
        for (idx, entry) in self.flowgraphs.iter().enumerate() {
            let content = std::fs::read_to_string(&entry.toml_path)
                .map_err(|e| format!("cannot read '{}': {e}", entry.toml_path))?;
            let def: FlowgraphDef = toml::from_str(&content)
                .map_err(|e| format!("invalid TOML '{}': {e}", entry.toml_path))?;
            for port in &def.ports {
                port_types.insert((idx, port.id.clone()), port.stream_type.clone());
            }
            parsed_defs.push(def);
        }

        // Head wiring + radio provisioning.
        //
        //   1. `add_head(...)` was used → the head is a real permanent FG
        //      (already in `self.flowgraphs`). We just auto-add a stream
        //      Connection from the head's `output_port` to the first
        //      swappable's c32 input port. Freq/gain retunes happen via
        //      live messages dispatched to the head's `sdr_block` — no
        //      separate RadioController is involved.
        //
        //   2. Back-compat: if no head was registered AND no manual radio
        //      was attached AND no external radio buffer was wired in,
        //      auto-build a `RadioController` from the first protocol's
        //      [radio]. Skipped entirely when (1) applies.
        let mut head_sdr_block: Option<String> = None;
        if let Some(head_idx) = self.head_fg_idx {
            let head_path = &self.flowgraphs[head_idx].toml_path;
            let content = std::fs::read_to_string(head_path)
                .map_err(|e| format!("cannot read head '{head_path}': {e}"))?;
            let head_file: HeadFile = toml::from_str(&content)
                .map_err(|e| format!("invalid head TOML '{head_path}': {e}"))?;
            let h = head_file.head;
            head_sdr_block = h.sdr_block.clone();
            let output_port = h.output_port.clone()
                .ok_or_else(|| format!("head '{head_path}': [head] needs output_port"))?;

            // Find the first swappable that explicitly marks an input port
            // as `from = "head"`. The wiring is named — no heuristic about
            // "first c32 input port" — so flows can have any number of
            // input ports without ambiguity.
            let (swap_idx, swap_port_id) = parsed_defs
                .iter()
                .enumerate()
                .filter(|(i, _)| !self.flowgraphs[*i].permanent)
                .find_map(|(i, def)| {
                    def.ports.iter()
                        .find(|p| matches!(p.direction, PortDirection::In)
                            && p.from.as_deref() == Some("head"))
                        .map(|p| (i, p.id.clone()))
                })
                .ok_or_else(|| format!(
                    "head '{}' registered but no swappable flow has an input port with `from = \"head\"`",
                    self.flowgraphs[head_idx].toml_path
                ))?;

            self.connections.push(Connection {
                from_fg: head_idx,
                from_port: output_port,
                to_fg: swap_idx,
                to_port: swap_port_id,
            });

            // Validate: every swappable's [radio] sample_rate_hz agrees.
            // Head's resampler ratio is fixed at build time.
            let mut declared: Option<(f64, &str)> = None;
            for (idx, def) in parsed_defs.iter().enumerate() {
                if self.flowgraphs[idx].permanent { continue; }
                let Some(r) = def.radio.as_ref() else { continue };
                let Some(rate) = r.sample_rate_hz else { continue };
                match declared {
                    None => declared = Some((rate, &self.flowgraphs[idx].toml_path)),
                    Some((prev, prev_path)) if (prev - rate).abs() > 1.0 => {
                        return Err(format!(
                            "flow '{}' demands sample_rate_hz={rate}, but '{prev_path}' \
                             demands {prev}; head's resampler ratio is fixed at startup",
                            self.flowgraphs[idx].toml_path
                        ).into());
                    }
                    _ => {}
                }
            }
        }
        // Note: protocol-only `[radio]` (no head, no `attach_radio`,
        // no `connect_radio`) is no longer auto-provisioned — register a
        // head TOML via `add_head(...)` instead.

        let mut channels: HashMap<String, SharedBuf> = HashMap::new();
        for conn in &self.connections {
            let from_key = format!("{}:{}", conn.from_fg, conn.from_port);
            let to_key   = format!("{}:{}", conn.to_fg,   conn.to_port);
            let stream_type = port_types
                .get(&(conn.from_fg, conn.from_port.clone()))
                .or_else(|| port_types.get(&(conn.to_fg, conn.to_port.clone())))
                .unwrap_or(&StreamType::C32);
            let buf = SharedBuf::new(stream_type);
            channels.insert(from_key, buf.clone());
            channels.insert(to_key,   buf);
        }

        // Insert RadioController output buffers as C32 channels
        let mut radio_bufs: Vec<(usize, crate::radio_controller::RadioOutputBuf)> = Vec::new();
        for ri in &self.radio_inputs {
            let to_key = format!("{}:{}", ri.to_fg, ri.to_port);
            channels.insert(to_key, SharedBuf::C32(ri.buf.clone()));
            radio_bufs.push((ri.to_fg, ri.buf.clone()));
        }

        let mut registry = PluginRegistry::new(&self.plugin_dir);
        let custom_parsers = self.custom_parsers;

        for entry in &self.flowgraphs {
            let content = std::fs::read_to_string(&entry.toml_path)
                .map_err(|e| format!("cannot read '{}': {e}", entry.toml_path))?;
            let def: FlowgraphDef = toml::from_str(&content)
                .map_err(|e| format!("invalid TOML '{}': {e}", entry.toml_path))?;
            for block in &def.blocks {
                registry.ensure_loaded(&block.plugin)
                    .map_err(|e| format!("'{}': {e}", entry.toml_path))?;
            }
        }

        let entries: Vec<(usize, String, bool)> = self.flowgraphs.iter()
            .enumerate()
            .map(|(i, e)| (i, e.toml_path.clone(), e.permanent))
            .collect();

        let mut ctrl = FlowgraphController::new(
            registry, channels, custom_parsers, self.connections, radio_bufs,
        );
        ctrl.radio = self.radio;
        ctrl.tap_sender = self.tap_channel.map(|(tx, _)| tx);
        ctrl.head_fg_idx = self.head_fg_idx;
        ctrl.head_sdr_block = head_sdr_block;

        let rt = Runtime::new();
        let rt_handle = rt.handle();

        // Auto-start the attached radio (if any) before yielding to the
        // user closure, so callers don't need to mention RadioController.
        // `start_radio` is a no-op when no radio was attached or when the
        // radio is already running.
        let rt_for_closure = rt_handle.clone();
        rt.block_on(async move {
            ctrl.start_radio(&rt_handle).await?;
            f(ctrl, rt_for_closure, entries).await
        })
    }

    /// Build, start, and run the full system with UDP control. Blocks until shutdown.
    pub fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let udp_port = self.udp_port.unwrap_or(7878);

        let socket = std::net::UdpSocket::bind(format!("0.0.0.0:{udp_port}"))
            .map_err(|e| format!("bind UDP :{udp_port}: {e}"))?;
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

        println!("Listening for UDP commands on port {udp_port}");
        println!("  -s <path.toml>              swap first swappable FG");
        println!("  -s <fg_index> <path.toml>   swap specific FG by index");
        println!("  Q                           quit\n");

        self.run_with(move |mut ctrl, rt_handle, entries| async move {
            // Start permanent FGs
            for &(idx, ref toml_path, permanent) in &entries {
                if permanent {
                    println!("Starting permanent flowgraph {idx} from '{toml_path}' ...");
                    ctrl.start_permanent(idx, toml_path, &rt_handle).await?;
                    println!("  fg/{idx}/ running.");
                }
            }

            ctrl.activate_selectors().await?;

            // Start swappable FGs
            for &(idx, ref toml_path, permanent) in &entries {
                if !permanent {
                    println!("Starting swappable flowgraph {idx} from '{toml_path}' ...");
                    ctrl.start_swappable(idx, toml_path, &rt_handle).await?;
                    println!("  fg/{idx}/ running.");
                }
            }

            println!("\nAll flowgraphs running.\n");

            let first_swappable_idx = entries.iter()
                .find(|(_, _, perm)| !perm)
                .map(|(i, _, _)| *i);

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
            Ok(())
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
    channels: HashMap<String, SharedBuf>,
    custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
    connections: Vec<Connection>,
    /// External radio buffers: (fg_idx, buf) — cleared during swap.
    radio_bufs: Vec<(usize, crate::radio_controller::RadioOutputBuf)>,
    perm_handles: HashMap<usize, FlowgraphHandle>,
    perm_block_ids: HashMap<usize, HashMap<String, BlockId>>,
    perm_port_defs: HashMap<usize, Vec<PortDef>>,
    selector_infos: Vec<SelectorInfo>,
    swap_states: HashMap<usize, SwappableState>,
    /// Optional RadioController — auto-retuned on swap when the target TOML
    /// declares a `[radio]` section. Used when `add_head` was *not* called
    /// (back-compat with `attach_radio` / `connect_radio`).
    radio: Option<crate::radio_controller::RadioController>,
    /// Optional sender cloned into every `NamedMessagePipe` block that the
    /// controller injects for `[[controller_taps]]` entries.
    tap_sender: Option<mpsc::Sender<(String, Pmt)>>,
    /// Index of the head flowgraph (set by `add_head`). When present,
    /// freq/gain retunes are dispatched to the head's `sdr_block` via
    /// `perm_handles[head_fg_idx]`.
    head_fg_idx: Option<usize>,
    /// Block id (in TOML) within the head flowgraph that responds to
    /// `freq`/`gain` messages. Looked up against `perm_block_ids` at
    /// dispatch time.
    head_sdr_block: Option<String>,
}

impl FlowgraphController {
    /// Create a builder for configuring the controller.
    pub fn builder(plugin_dir: impl Into<String>) -> FlowgraphControllerBuilder {
        FlowgraphControllerBuilder {
            plugin_dir: plugin_dir.into(),
            flowgraphs: Vec::new(),
            connections: Vec::new(),
            radio_inputs: Vec::new(),
            udp_port: None,
            custom_parsers: HashMap::new(),
            radio: None,
            tap_channel: None,
            head_path: None,
            head_fg_idx: None,
        }
    }

    pub fn new(
        registry: PluginRegistry,
        channels: HashMap<String, SharedBuf>,
        custom_parsers: HashMap<String, fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>>,
        connections: Vec<Connection>,
        radio_bufs: Vec<(usize, crate::radio_controller::RadioOutputBuf)>,
    ) -> Self {
        Self {
            registry, channels, custom_parsers, connections, radio_bufs,
            perm_handles: HashMap::new(),
            perm_block_ids: HashMap::new(),
            perm_port_defs: HashMap::new(),
            selector_infos: Vec::new(),
            swap_states: HashMap::new(),
            radio: None,
            tap_sender: None,
            head_fg_idx: None,
            head_sdr_block: None,
        }
    }

    /// Access the attached RadioController (if any).
    pub fn radio_mut(&mut self) -> Option<&mut crate::radio_controller::RadioController> {
        self.radio.as_mut()
    }

    /// Start the attached RadioController, if present. Idempotent: returns
    /// `Ok(())` when no radio was attached.
    pub async fn start_radio(
        &mut self,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(r) = self.radio.as_mut() {
            r.start(rt_handle).await?;
        }
        Ok(())
    }

    /// Access the plugin registry (e.g. to pre-load plugins for swap targets).
    pub fn registry_mut(&mut self) -> &mut PluginRegistry {
        &mut self.registry
    }

    pub async fn start_permanent(
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

        self.perm_block_ids.insert(fg_idx, block_ids);
        self.perm_port_defs.insert(fg_idx, port_defs);
        self.perm_handles.insert(fg_idx, handle);
        Ok(())
    }

    pub async fn activate_selectors(
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

    pub async fn start_swappable(
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

        // Apply this flow's [radio] demand to the radio (head FG or legacy
        // RadioController), so the first swappable's tuning is live as soon
        // as its DSP starts.
        let content = std::fs::read_to_string(toml_path)
            .map_err(|e| format!("cannot re-read '{toml_path}' for [radio]: {e}"))?;
        let def: FlowgraphDef = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML in '{toml_path}': {e}"))?;
        if let Some(rsec) = def.radio.as_ref() {
            self.apply_radio_demand(rsec).await?;
        }
        Ok(())
    }

    /// Dispatch a `[radio]` demand to whichever radio is wired up:
    ///
    /// - **head-FG path** (`add_head` was used): live `freq`/`gain`
    ///   messages are sent to the head's `sdr_block`.
    /// - **legacy path** (`attach_radio` / auto-provisioned `RadioController`):
    ///   `set_frequency` / `set_gain` are called on the controller.
    ///
    /// `sample_rate_hz` in the demand is *not* live-applied (resampler is
    /// fixed at startup); the builder validates rate-consistency across
    /// all swappables when a head is registered.
    async fn apply_radio_demand(
        &mut self,
        rsec: &RadioSectionDef,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let (Some(head_idx), Some(sdr_block_name)) =
            (self.head_fg_idx, self.head_sdr_block.clone())
        {
            let block_id = self.perm_block_ids.get(&head_idx)
                .and_then(|m| m.get(&sdr_block_name).copied())
                .ok_or_else(|| format!(
                    "head sdr_block '{sdr_block_name}' not found in fg/{head_idx}/"
                ))?;
            let handle = self.perm_handles.get_mut(&head_idx)
                .ok_or_else(|| format!(
                    "head fg/{head_idx}/ not started — apply_radio_demand called too early"
                ))?;
            if let Some(f) = rsec.frequency_hz {
                handle.callback(block_id, "freq", Pmt::F64(f)).await
                    .map_err(|e| format!("retune freq via head: {e}"))?;
            }
            if let Some(g) = rsec.gain_db {
                handle.callback(block_id, "gain", Pmt::F64(g)).await
                    .map_err(|e| format!("retune gain via head: {e}"))?;
            }
        } else if let Some(radio) = self.radio.as_mut() {
            if let Some(f) = rsec.frequency_hz {
                radio.set_frequency(f).await
                    .map_err(|e| format!("retune radio freq: {e}"))?;
            }
            if let Some(g) = rsec.gain_db {
                radio.set_gain(g).await
                    .map_err(|e| format!("retune radio gain: {e}"))?;
            }
        }
        Ok(())
    }

    pub async fn swap(
        &mut self,
        fg_idx: usize,
        new_toml: &str,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use std::time::Instant;

        let t_swap = Instant::now();

        let state = self.swap_states.get(&fg_idx)
            .ok_or_else(|| format!("unknown swappable fg/{fg_idx}/"))?;
        let prev_toml = state.current_toml.clone();

        // 1. Park selectors connected to this FG (if any)
        let t_step = Instant::now();
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
        println!("    [swap] 1-park_selectors:  {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);

        // 2. Terminate old FG
        let t_step = Instant::now();
        self.swap_states.get_mut(&fg_idx).unwrap()
            .handle.terminate_and_wait().await
            .map_err(|e| format!("terminate fg/{fg_idx}/: {e}"))?;
        println!("    [swap] 2-terminate:       {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);

        // 3. Clear connected buffers
        let t_step = Instant::now();
        for conn in &self.connections {
            if conn.to_fg == fg_idx {
                let from_key = format!("{}:{}", conn.from_fg, conn.from_port);
                if let Some(buf) = self.channels.get(&from_key) {
                    buf.clear();
                }
            }
        }
        for (ri_fg, ri_buf) in &self.radio_bufs {
            if *ri_fg == fg_idx {
                ri_buf.lock().unwrap().clear();
            }
        }
        println!("    [swap] 3-clear_buffers:   {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);

        // 4. Load new plugins if needed
        let t_step = Instant::now();
        let content = std::fs::read_to_string(new_toml)
            .map_err(|e| format!("cannot read '{new_toml}': {e}"))?;
        let def: FlowgraphDef = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML '{new_toml}': {e}"))?;
        for block in &def.blocks {
            self.registry.ensure_loaded(&block.plugin)
                .map_err(|e| format!("swap fg/{fg_idx}/: {e}"))?;
        }
        println!("    [swap] 4-load_plugins:    {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);

        // 5. Build & start (with fallback)
        let t_step = Instant::now();
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
        println!("    [swap] 5-build_and_start: {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);

        // 6. Unpark selectors
        let t_step = Instant::now();
        for &idx in &connected_sels {
            let sel = &self.selector_infos[idx];
            let handle = self.perm_handles.get_mut(&sel.perm_fg).unwrap();
            handle.callback(sel.block_id, "output_index", Pmt::U32(1)).await
                .map_err(|e| format!("unpark selector: {e}"))?;
        }
        println!("    [swap] 6-unpark_selectors: {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);

        // 7. Auto-retune the radio from the new TOML's [radio] section.
        if let Some(rsec) = def.radio.as_ref() {
            let t_step = Instant::now();
            self.apply_radio_demand(rsec).await?;
            println!("    [swap] 7-retune_radio:    {:.3} ms", t_step.elapsed().as_secs_f64() * 1000.0);
        }

        println!("    [swap] total:             {:.3} ms", t_swap.elapsed().as_secs_f64() * 1000.0);

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

    pub async fn shutdown_all(&mut self) {
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
        if let Some(r) = self.radio.as_mut() {
            r.shutdown().await;
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

            if conn.message {
                fg.connect_message(src_id, src_port, dst_id, dst_port)
                    .map_err(|e| format!("msg {src_block}.{src_port} -> {dst_block}.{dst_port}: {e}"))?;
            } else {
                fg.connect_dyn(src_id, src_port, dst_id, dst_port)
                    .map_err(|e| format!("connect {src_block}.{src_port} -> {dst_block}.{dst_port}: {e}"))?;
            }
        }

        // 2b. Inject NamedMessagePipe blocks for each [[controller_taps]] entry.
        //     Each pipe forwards tapped PMTs into the controller's tap channel,
        //     tagged with the tap's name. Silently no-op if no tap channel is
        //     attached OR if the tap's src block doesn't exist in this FG.
        if let Some(tx) = self.tap_sender.as_ref() {
            for tap in &def.controller_taps {
                let (src_block, src_port) = tap.src.split_once('.')
                    .ok_or_else(|| format!("invalid tap src '{}', expected 'block.port'", tap.src))?;
                let Some(&src_id) = block_ids.get(src_block) else {
                    // Silently drop: new flow doesn't declare this tap's block.
                    continue;
                };
                let name = tap.name.clone();
                let sender = tx.clone();
                let pipe_id = fg.add_block_dyn(move |id| {
                    Box::new(WrappedKernel::new(
                        bridge::NamedMessagePipe::new(name, sender),
                        id,
                    ))
                });
                fg.connect_message(src_id, src_port, pipe_id, "in")
                    .map_err(|e| format!("tap {}.{} -> pipe '{}': {e}",
                        src_block, src_port, tap.name))?;
            }
        }

        // 3. Auto-inject bridge blocks for each port.
        //    The SharedBuf already carries the correct element type — no need
        //    to re-read stream_type from the port definition here.
        for port in &def.ports {
            let channel_key = format!("{fg_idx}:{}", port.id);
            let buf = self.channels.get(&channel_key)
                .ok_or_else(|| format!(
                    "no channel for '{channel_key}' — check connect() calls"
                ))?
                .clone();

            match port.direction {
                PortDirection::Out => {
                    // Auto-inject bridge sink: src_block.src_port -> bridge_sink.input
                    let src_spec = port.src.as_ref()
                        .ok_or_else(|| format!("output port '{}' missing 'src' field", port.id))?;
                    let (src_block, src_port) = src_spec.split_once('.')
                        .ok_or_else(|| format!("invalid port src '{src_spec}', expected 'block.port'"))?;
                    let &src_id = block_ids.get(src_block)
                        .ok_or_else(|| format!("unknown block '{src_block}' in port '{}'", port.id))?;

                    let bridge_id = add_bridge_sink(&mut fg, buf);
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

                    let bridge_id = add_bridge_source(&mut fg, buf);
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
// The element type is encoded in SharedBuf — no separate stream_type arg.

fn add_bridge_sink(fg: &mut Flowgraph, buf: SharedBuf) -> BlockId {
    match buf {
        SharedBuf::U8(b) => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSinkU8::new(b), id))
        }),
        SharedBuf::F32(b) => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSinkF32::new(b), id))
        }),
        SharedBuf::C32(b) => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSinkC32::new(b), id))
        }),
    }
}

fn add_bridge_source(fg: &mut Flowgraph, buf: SharedBuf) -> BlockId {
    match buf {
        SharedBuf::U8(b) => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSourceU8::new(b), id))
        }),
        SharedBuf::F32(b) => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSourceF32::new(b), id))
        }),
        SharedBuf::C32(b) => fg.add_block_dyn(|id| {
            Box::new(WrappedKernel::new(bridge::BridgeSourceC32::new(b), id))
        }),
    }
}
