//! RadioController — owns SDR hardware + resampler, exposes a single IQ output.
//!
//! The SDR runs at a fixed `hardware_rate`. A polyphase FIR resampler decimates
//! to a fixed `output_rate`. The entire chain is built once in [`start`] and
//! kept alive for the whole lifetime of the controller — swaps of the
//! downstream protocol flowgraph do NOT tear it down. Per-swap retuning is
//! done via live message callbacks (`set_frequency`, `set_gain`), which the
//! SDR block handles without interrupting the stream.
//!
//! ```text
//!   RadioController (persistent internal flowgraph)
//!   ┌──────────────────────────────────────────────────┐
//!   │  SeifySource ──→ FirResampler ──→ BridgeSinkC32  │──→ output_buf
//!   └──────────────────────────────────────────────────┘
//!                                                            ↓
//!                                              FlowgraphController input
//! ```
//!
//! Protocols that need a different rate than `output_rate` must prepend their
//! own resampler inside their TOML flowgraph definition.

use crate::bridge;
use crate::{LoadedPlugin, plugin_path};
use futuresdr::prelude::Complex32;
use futuresdr::runtime::{BlockId, Flowgraph, FlowgraphHandle, Pmt, RuntimeHandle, WrappedKernel};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Shared C32 buffer between RadioController output and FlowgraphController input.
pub type RadioOutputBuf = Arc<Mutex<VecDeque<Complex32>>>;

/// Controls SDR hardware + resampler as a single unit.
pub struct RadioController {
    // SDR config
    device_args: String,
    frequency_hz: f64,
    hardware_rate: f64,
    output_rate: f64,
    gain_db: f64,

    // Plugin dir for loading seify_source_plugin + fir_resampler_plugin
    plugin_dir: String,

    // Shared output buffer (C32)
    output_buf: RadioOutputBuf,

    // Runtime state (set after start)
    state: Option<RadioState>,
}

struct RadioState {
    handle: FlowgraphHandle,
    sdr_block_id: BlockId,
    /// Keep plugin libraries alive while the flowgraph holds references to block vtables.
    _plugins: Vec<LoadedPlugin>,
}

impl RadioController {
    /// Create a new RadioController. Returns `(controller, output_buffer)`.
    ///
    /// The `output_buf` should be connected to a FlowgraphController bridge source.
    pub fn new(
        plugin_dir: &str,
        device_args: &str,
        frequency_hz: f64,
        hardware_rate: f64,
        output_rate: f64,
        gain_db: f64,
    ) -> (Self, RadioOutputBuf) {
        let output_buf: RadioOutputBuf = Arc::new(Mutex::new(VecDeque::new()));
        let ctrl = Self {
            device_args: device_args.to_string(),
            frequency_hz,
            hardware_rate,
            output_rate,
            gain_db,
            plugin_dir: plugin_dir.to_string(),
            output_buf: output_buf.clone(),
            state: None,
        };
        (ctrl, output_buf)
    }

    /// Start the persistent internal flowgraph:
    /// SeifySource → FirResampler → BridgeSinkC32. Idempotent: a no-op if
    /// the controller is already running.
    pub async fn start(
        &mut self,
        rt_handle: &RuntimeHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.state.is_some() {
            return Ok(());
        }
        let (fg, sdr_block_id, plugins) = self.build_flowgraph()?;
        let handle = rt_handle.start(fg).await
            .map_err(|e| format!("RadioController: start failed: {e}"))?;
        self.state = Some(RadioState { handle, sdr_block_id, _plugins: plugins });
        Ok(())
    }

    /// Set the center frequency. **Fire-and-forget**: clones the SDR block
    /// handle and dispatches the `freq` message on a background thread, then
    /// returns immediately. The caller does not pay the hardware retune
    /// latency; the SDR settles on its own. Errors from the dispatched
    /// callback are reported via stderr (not via this function's Result).
    pub async fn set_frequency(
        &mut self,
        freq_hz: f64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.frequency_hz = freq_hz;
        let state = self.state.as_mut()
            .ok_or("RadioController: not started")?;
        let mut handle = state.handle.clone();
        let sdr_block_id = state.sdr_block_id;
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            match futuresdr::async_io::block_on(
                handle.callback(sdr_block_id, "freq", Pmt::F64(freq_hz))
            ) {
                Ok(_) => println!(
                    "        [RadioController async retune] freq {:.3} MHz settled in {:.3} ms",
                    freq_hz / 1e6,
                    t.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => eprintln!("[RadioController async retune] freq {freq_hz} failed: {e}"),
            }
        });
        Ok(())
    }

    /// Set the gain. **Fire-and-forget** (same semantics as
    /// [`set_frequency`]): dispatches the `gain` message on a background
    /// thread and returns immediately.
    pub async fn set_gain(
        &mut self,
        gain_db: f64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.gain_db = gain_db;
        let state = self.state.as_mut()
            .ok_or("RadioController: not started")?;
        let mut handle = state.handle.clone();
        let sdr_block_id = state.sdr_block_id;
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            match futuresdr::async_io::block_on(
                handle.callback(sdr_block_id, "gain", Pmt::F64(gain_db))
            ) {
                Ok(_) => println!(
                    "        [RadioController async retune] gain {gain_db:.2} dB settled in {:.3} ms",
                    t.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => eprintln!("[RadioController async retune] gain {gain_db} failed: {e}"),
            }
        });
        Ok(())
    }

    /// Current center frequency in Hz.
    pub fn frequency(&self) -> f64 { self.frequency_hz }

    /// Current hardware sample rate in Hz.
    pub fn hardware_rate(&self) -> f64 { self.hardware_rate }

    /// Current output bandwidth in Hz (fixed for the lifetime of the controller).
    pub fn output_rate(&self) -> f64 { self.output_rate }

    /// Current gain in dB.
    pub fn gain(&self) -> f64 { self.gain_db }

    /// Reference to the shared output buffer (for connecting to FlowgraphController).
    pub fn output_buf(&self) -> &RadioOutputBuf { &self.output_buf }

    /// Shutdown the persistent internal flowgraph.
    pub async fn shutdown(&mut self) {
        if let Some(mut state) = self.state.take() {
            if let Err(e) = state.handle.terminate_and_wait().await {
                eprintln!("RadioController: shutdown: {e}");
            }
        }
    }

    // ── Internal ──────────────────────────────────────────────────────

    fn build_flowgraph(
        &self,
    ) -> Result<(Flowgraph, BlockId, Vec<LoadedPlugin>), Box<dyn std::error::Error + Send + Sync>> {
        let mut fg = Flowgraph::new();

        // Load plugins
        let sdr_plugin = unsafe {
            LoadedPlugin::load(plugin_path(&self.plugin_dir, "seify_source_plugin").to_str().unwrap())
        };
        let resampler_plugin = unsafe {
            LoadedPlugin::load(plugin_path(&self.plugin_dir, "fir_resampler_plugin").to_str().unwrap())
        };

        // SeifySource: (device_args, frequency_hz, sample_rate_hz, gain_db)
        let sdr_config: Box<dyn std::any::Any + Send> = Box::new((
            self.device_args.clone(),
            self.frequency_hz,
            self.hardware_rate,
            self.gain_db,
        ));
        let sdr_id = fg.add_block_dyn(sdr_plugin.prepare(sdr_config));

        // FirResampler: (interp, decim) — rational approximation of output_rate / hardware_rate
        let (interp, decim) = rational_approx(self.output_rate, self.hardware_rate);
        let resampler_config: Box<dyn std::any::Any + Send> = Box::new((interp, decim));
        let resampler_id = fg.add_block_dyn(resampler_plugin.prepare(resampler_config));

        // BridgeSinkC32: writes to the shared output buffer
        let bridge_id = fg.add_block_dyn({
            let buf = self.output_buf.clone();
            |id| Box::new(WrappedKernel::new(bridge::BridgeSinkC32::new(buf), id))
        });

        // Wire: SDR → Resampler → BridgeSink
        fg.connect_dyn(sdr_id, "outputs[0]", resampler_id, "input")
            .map_err(|e| format!("RadioController: SDR→Resampler: {e}"))?;
        fg.connect_dyn(resampler_id, "output", bridge_id, "input")
            .map_err(|e| format!("RadioController: Resampler→Bridge: {e}"))?;

        Ok((fg, sdr_id, vec![sdr_plugin, resampler_plugin]))
    }
}

/// Compute rational interp/decim to approximate `num / den`.
fn rational_approx(num: f64, den: f64) -> (usize, usize) {
    let ratio = num / den;

    if ratio >= 1.0 {
        // Upsampling
        let interp = (ratio * 100.0).round() as usize;
        let decim = 100;
        let g = gcd(interp, decim);
        return (interp / g, decim / g);
    }

    // Downsampling — invert and swap
    let inv = den / num;
    let decim = (inv * 100.0).round() as usize;
    let interp = 100;
    let g = gcd(interp, decim);
    (interp / g, decim / g)
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}
