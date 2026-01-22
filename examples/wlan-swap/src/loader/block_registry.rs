//! Block Registry for TOML Loader
//! 
//! Provides block factories and registration for instantiating blocks from TOML configs.

use anyhow::{Result, Context, bail};
use futuresdr::prelude::*;
use futuresdr::blocks::{Apply, NullSource, NullSink, Delay, Fft, Combine, Throttle};
#[cfg(not(target_arch = "wasm32"))]
use futuresdr::blocks::{WebsocketPmtSink, FileSource, BlobToUdp};
#[cfg(not(target_arch = "wasm32"))]
use futuresdr::blocks::seify::Builder;
use crate::zigbee::{Mac, IqDelay, ClockRecoveryMm, Decoder, modulator};
use crate::wifi;
use super::toml_loader::{BlockConfig, ParameterConfig};

/// Block factory trait
pub trait BlockFactory: Send + Sync {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId>;
}

/// Block registry that maps block types to factories
pub struct BlockRegistry {
    factories: std::collections::HashMap<String, Box<dyn BlockFactory>>,
}

impl BlockRegistry {
    /// Create a new block registry with default factories
    pub fn new() -> Self {
        let mut registry = Self {
            factories: std::collections::HashMap::new(),
        };
        
        // Register ZigBee blocks
        registry.register("zigbee::Mac", Box::new(MacFactory));
        registry.register("zigbee::Modulator", Box::new(ModulatorFactory));
        registry.register("zigbee::IqDelay", Box::new(IqDelayFactory));
        registry.register("zigbee::ClockRecoveryMm", Box::new(ClockRecoveryMmFactory));
        registry.register("zigbee::Decoder", Box::new(DecoderFactory));
        
        // Register generic blocks
        registry.register("Apply", Box::new(ApplyFactory));
        registry.register("Combine", Box::new(CombineFactory));
        registry.register("Delay", Box::new(DelayFactory));
        registry.register("Fft", Box::new(FftFactory));
        registry.register("Throttle", Box::new(ThrottleFactory));
        #[cfg(not(target_arch = "wasm32"))]
        registry.register("WebsocketPmtSink", Box::new(WebsocketPmtSinkFactory));
        #[cfg(not(target_arch = "wasm32"))]
        registry.register("FileSource", Box::new(FileSourceFactory));
        #[cfg(not(target_arch = "wasm32"))]
        registry.register("BlobToUdp", Box::new(BlobToUdpFactory));
        registry.register("NullSource", Box::new(NullSourceFactory));
        registry.register("NullSink", Box::new(NullSinkFactory));
        
        // Register WiFi blocks
        registry.register("wifi::Mac", Box::new(WifiMacFactory));
        registry.register("wifi::Encoder", Box::new(WifiEncoderFactory));
        registry.register("wifi::Mapper", Box::new(WifiMapperFactory));
        registry.register("wifi::Prefix", Box::new(WifiPrefixFactory));
        registry.register("wifi::MovingAverage", Box::new(WifiMovingAverageFactory));
        registry.register("wifi::SyncShort", Box::new(WifiSyncShortFactory));
        registry.register("wifi::SyncLong", Box::new(WifiSyncLongFactory));
        registry.register("wifi::FrameEqualizer", Box::new(WifiFrameEqualizerFactory));
        registry.register("wifi::Decoder", Box::new(WifiDecoderFactory));
        
        // Register SDR hardware blocks (seify)
        #[cfg(not(target_arch = "wasm32"))]
        registry.register("seify::Source", Box::new(SeifySourceFactory));
        #[cfg(not(target_arch = "wasm32"))]
        registry.register("seify::Sink", Box::new(SeifySinkFactory));
        
        // Register control blocks
        registry.register("FlowgraphController", Box::new(FlowgraphControllerFactory));
        
        registry
    }
    
    /// Register a block factory
    pub fn register(&mut self, block_type: &str, factory: Box<dyn BlockFactory>) {
        self.factories.insert(block_type.to_string(), factory);
    }
    
    /// Create a block from configuration
    pub fn create_block(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let factory = self.factories.get(&config.block_type)
            .with_context(|| format!("No factory registered for block type: {}", config.block_type))?;
        
        factory.create(fg, config)
    }
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// Helper functions to extract parameters
fn get_param_u32(params: &[ParameterConfig], name: &str) -> Result<u32> {
    params.iter()
        .find(|p| p.name == name)
        .and_then(|p| p.value.as_integer())
        .map(|v| v as u32)
        .with_context(|| format!("Parameter '{}' not found or invalid", name))
}

fn get_param_f32(params: &[ParameterConfig], name: &str) -> Result<f32> {
    params.iter()
        .find(|p| p.name == name)
        .and_then(|p| p.value.as_float())
        .map(|v| v as f32)
        .with_context(|| format!("Parameter '{}' not found or invalid", name))
}

fn get_param_f64(params: &[ParameterConfig], name: &str) -> Result<f64> {
    params.iter()
        .find(|p| p.name == name)
        .and_then(|p| p.value.as_float())
        .with_context(|| format!("Parameter '{}' not found or invalid", name))
}

fn get_param_string(params: &[ParameterConfig], name: &str) -> Result<String> {
    params.iter()
        .find(|p| p.name == name)
        .and_then(|p| p.value.as_str())
        .map(|v| v.to_string())
        .with_context(|| format!("Parameter '{}' not found or invalid", name))
}

// ============================================================================
// Block Factories
// ============================================================================

/// Factory for zigbee::Mac
struct MacFactory;

impl BlockFactory for MacFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        let mac: Mac = Mac::new();
        Ok(fg.add_block(mac).into())
    }
}

/// Factory for zigbee::Modulator (composite block)
struct ModulatorFactory;

impl BlockFactory for ModulatorFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        // The modulator function returns a BlockId after adding the composite to the flowgraph
        Ok(modulator(fg))
    }
}

/// Factory for zigbee::IqDelay
struct IqDelayFactory;

impl BlockFactory for IqDelayFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        // Note: IqDelay is hardcoded with 40 samples delay in the implementation
        let iq_delay: IqDelay = IqDelay::new();
        Ok(fg.add_block(iq_delay).into())
    }
}

/// Factory for zigbee::ClockRecoveryMm
struct ClockRecoveryMmFactory;

impl BlockFactory for ClockRecoveryMmFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let omega = get_param_f32(&config.parameters, "omega")?;
        let gain_omega = get_param_f32(&config.parameters, "gain_omega")?;
        let mu = get_param_f32(&config.parameters, "mu")?;
        let gain_mu = get_param_f32(&config.parameters, "gain_mu")?;
        let omega_relative_limit = get_param_f32(&config.parameters, "omega_relative_limit")?;
        
        let mm: ClockRecoveryMm = ClockRecoveryMm::new(omega, gain_omega, mu, gain_mu, omega_relative_limit);
        Ok(fg.add_block(mm).into())
    }
}

/// Factory for zigbee::Decoder
struct DecoderFactory;

impl BlockFactory for DecoderFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let threshold = get_param_u32(&config.parameters, "threshold")?;
        
        let decoder: Decoder = Decoder::new(threshold);
        Ok(fg.add_block(decoder).into())
    }
}

/// Factory for Apply blocks with predefined closures
struct ApplyFactory;

impl BlockFactory for ApplyFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        // Get the closure name from parameters
        let closure_name = get_param_string(&config.parameters, "function")?;
        
        match closure_name.as_str() {
            "phase_detector_iir" => {
                // Create the phase detector with IIR filter
                let alpha = get_param_f32(&config.parameters, "alpha")
                    .unwrap_or(0.00016);
                
                let mut last = Complex32::new(0.0, 0.0);
                let mut iir: f32 = 0.0;
                
                let block = Apply::<_, _, _>::new(move |i: &Complex32| -> f32 {
                    let phase = (last.conj() * i).arg();
                    last = *i;
                    iir = (1.0 - alpha) * iir + alpha * phase;
                    phase - iir
                });
                
                Ok(fg.add_block(block).into())
            }
            "norm_sqr" => {
                // Complex32 -> f32: |z|^2 = z.norm_sqr()
                let block = Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr());
                Ok(fg.add_block(block).into())
            }
            "dc_offset_removal" => {
                // DC offset removal using IIR filter
                let ratio = 1.0e-5f32;
                let mut avg_real = 0.0f32;
                let mut avg_img = 0.0f32;
                
                let block = Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
                    avg_real = ratio * (c.re - avg_real) + avg_real;
                    avg_img = ratio * (c.im - avg_img) + avg_img;
                    Complex32::new(c.re - avg_real, c.im - avg_img)
                });
                Ok(fg.add_block(block).into())
            }
            _ => bail!("Unknown closure type for Apply block: {}", closure_name),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Factory for WebsocketPmtSink
struct WebsocketPmtSinkFactory;

#[cfg(not(target_arch = "wasm32"))]
impl BlockFactory for WebsocketPmtSinkFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let port = get_param_u32(&config.parameters, "port")?;
        
        let block = WebsocketPmtSink::new(port);
        Ok(fg.add_block(block).into())
    }
}

/// Factory for NullSource
struct NullSourceFactory;

impl BlockFactory for NullSourceFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        // Determine the data type from config
        let dtype = config.dtype.as_deref().unwrap_or("u8");
        
        match dtype {
            "u8" => Ok(fg.add_block(NullSource::<u8>::new()).into()),
            "u32" => Ok(fg.add_block(NullSource::<u32>::new()).into()),
            "f32" => Ok(fg.add_block(NullSource::<f32>::new()).into()),
            "Complex32" => Ok(fg.add_block(NullSource::<Complex32>::new()).into()),
            _ => bail!("Unsupported dtype for NullSource: {}", dtype),
        }
    }
}

/// Factory for NullSink
struct NullSinkFactory;

impl BlockFactory for NullSinkFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        // Determine the data type from config
        let dtype = config.dtype.as_deref().unwrap_or("u8");
        
        match dtype {
            "u8" => Ok(fg.add_block(NullSink::<u8>::new()).into()),
            "u32" => Ok(fg.add_block(NullSink::<u32>::new()).into()),
            "f32" => Ok(fg.add_block(NullSink::<f32>::new()).into()),
            "Complex32" => Ok(fg.add_block(NullSink::<Complex32>::new()).into()),
            _ => bail!("Unsupported dtype for NullSink: {}", dtype),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Factory for seify::Source (SDR hardware source)
struct SeifySourceFactory;

#[cfg(not(target_arch = "wasm32"))]
impl BlockFactory for SeifySourceFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let frequency = get_param_f64(&config.parameters, "frequency")?;
        let sample_rate = get_param_f64(&config.parameters, "sample_rate")?;
        let gain = get_param_f64(&config.parameters, "gain")?;
        
        let antenna = config.parameters.iter()
            .find(|p| p.name == "antenna")
            .and_then(|p| p.value.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        
        let args = config.parameters.iter()
            .find(|p| p.name == "args")
            .and_then(|p| p.value.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        
        let mut builder = Builder::new(args)?
            .frequency(frequency)
            .sample_rate(sample_rate)
            .gain(gain);
        
        if let Some(ant) = antenna {
            builder = builder.antenna(Some(ant));
        }
        
        let source = builder.build_source()?;
        Ok(fg.add_block(source).into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Factory for seify::Sink (SDR hardware sink)
struct SeifySinkFactory;

#[cfg(not(target_arch = "wasm32"))]
impl BlockFactory for SeifySinkFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let frequency = get_param_f64(&config.parameters, "frequency")?;
        let sample_rate = get_param_f64(&config.parameters, "sample_rate")?;
        let gain = get_param_f64(&config.parameters, "gain")?;
        
        let antenna = config.parameters.iter()
            .find(|p| p.name == "antenna")
            .and_then(|p| p.value.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        
        let args = config.parameters.iter()
            .find(|p| p.name == "args")
            .and_then(|p| p.value.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        
        let mut builder = Builder::new(args)?
            .frequency(frequency)
            .sample_rate(sample_rate)
            .gain(gain);
        
        if let Some(ant) = antenna {
            builder = builder.antenna(Some(ant));
        }
        
        let sink = builder.build_sink()?;
        Ok(fg.add_block(sink).into())
    }
}

/// FlowgraphController factory
struct FlowgraphControllerFactory;

impl BlockFactory for FlowgraphControllerFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        use crate::loader::flowgraph_controller::FlowgraphController;
        let block = FlowgraphController::new();
        Ok(fg.add_block(block).into())
    }
}

// ========================================
// Generic Blocks
// ========================================

/// Factory for Delay
struct DelayFactory;

impl BlockFactory for DelayFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let delay = get_param_u32(&config.parameters, "delay")? as isize;
        let dtype = config.dtype.as_deref().unwrap_or("Complex32");
        
        match dtype {
            "Complex32" => Ok(fg.add_block(Delay::<Complex32>::new(delay)).into()),
            "f32" => Ok(fg.add_block(Delay::<f32>::new(delay)).into()),
            "u8" => Ok(fg.add_block(Delay::<u8>::new(delay)).into()),
            _ => bail!("Unsupported dtype for Delay: {}", dtype),
        }
    }
}

/// Factory for Fft
struct FftFactory;

impl BlockFactory for FftFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        use futuresdr::blocks::FftDirection;
        
        let size = get_param_u32(&config.parameters, "size")? as usize;
        
        let direction = config.parameters.iter()
            .find(|p| p.name == "direction")
            .and_then(|p| p.value.as_str())
            .unwrap_or("Forward");
        
        let fft_dir = match direction {
            "Forward" => FftDirection::Forward,
            "Inverse" => FftDirection::Inverse,
            _ => FftDirection::Forward,
        };
        
        let normalize = config.parameters.iter()
            .find(|p| p.name == "normalize")
            .and_then(|p| p.value.as_bool())
            .unwrap_or(false);
        
        let scaling = config.parameters.iter()
            .find(|p| p.name == "scaling")
            .and_then(|p| p.value.as_float())
            .map(|v| v as f32);
        
        let fft: Fft = Fft::with_options(size, fft_dir, normalize, scaling);
        Ok(fg.add_block(fft).into())
    }
}

/// Factory for Throttle
struct ThrottleFactory;

impl BlockFactory for ThrottleFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let rate = get_param_f64(&config.parameters, "rate")?;
        let dtype = config.dtype.as_deref().unwrap_or("Complex32");
        
        match dtype {
            "Complex32" => Ok(fg.add_block(Throttle::<Complex32>::new(rate)).into()),
            "f32" => Ok(fg.add_block(Throttle::<f32>::new(rate)).into()),
            "u8" => Ok(fg.add_block(Throttle::<u8>::new(rate)).into()),
            _ => bail!("Unsupported dtype for Throttle: {}", dtype),
        }
    }
}

/// Factory for Combine
struct CombineFactory;

impl BlockFactory for CombineFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        // Try both 'closure' and 'function' parameter names
        let closure_name = config.parameters.iter()
            .find(|p| p.name == "closure" || p.name == "function")
            .and_then(|p| p.value.as_str())
            .context("Combine block requires 'closure' or 'function' parameter")?;
        
        match closure_name {
            "multiply_conj" | "mult_conjugate" => {
                // a * b.conj() : Complex32, Complex32 -> Complex32
                let combine: Combine<_, Complex32, Complex32, Complex32> = Combine::new(
                    |a: &Complex32, b: &Complex32| a * b.conj()
                );
                Ok(fg.add_block(combine).into())
            }
            "divide" => {
                // a / b : Complex32, Complex32 -> Complex32
                let combine: Combine<_, Complex32, Complex32, Complex32> = Combine::new(
                    |a: &Complex32, b: &Complex32| a / b
                );
                Ok(fg.add_block(combine).into())
            }
            "norm_divide" => {
                // a.norm() / b : Complex32, f32 -> f32
                let combine: Combine<_, Complex32, f32, f32> = Combine::new(
                    |a: &Complex32, b: &f32| a.norm() / b
                );
                Ok(fg.add_block(combine).into())
            }
            _ => bail!("Unknown closure type for Combine block: {}", closure_name),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Factory for FileSource
struct FileSourceFactory;

#[cfg(not(target_arch = "wasm32"))]
impl BlockFactory for FileSourceFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let path = config.parameters.iter()
            .find(|p| p.name == "path")
            .and_then(|p| p.value.as_str())
            .context("FileSource requires 'path' parameter")?;
        
        let repeat = config.parameters.iter()
            .find(|p| p.name == "repeat")
            .and_then(|p| p.value.as_bool())
            .unwrap_or(false);
        
        let dtype = config.dtype.as_deref().unwrap_or("Complex32");
        
        match dtype {
            "Complex32" => Ok(fg.add_block(FileSource::<Complex32>::new(path, repeat)).into()),
            "f32" => Ok(fg.add_block(FileSource::<f32>::new(path, repeat)).into()),
            "u8" => Ok(fg.add_block(FileSource::<u8>::new(path, repeat)).into()),
            _ => bail!("Unsupported dtype for FileSource: {}", dtype),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Factory for BlobToUdp
struct BlobToUdpFactory;

#[cfg(not(target_arch = "wasm32"))]
impl BlockFactory for BlobToUdpFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        // Try both 'address' and 'addr' parameter names
        let address = config.parameters.iter()
            .find(|p| p.name == "address" || p.name == "addr")
            .and_then(|p| p.value.as_str())
            .context("BlobToUdp requires 'address' or 'addr' parameter")?;
        
        Ok(fg.add_block(BlobToUdp::new(address)).into())
    }
}

// ========================================
// WiFi Blocks
// ========================================

/// Helper to parse MAC address from string
fn parse_mac_addr(s: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        bail!("Invalid MAC address format: {}", s);
    }
    let mut result = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        result[i] = u8::from_str_radix(part, 16)?;
    }
    Ok(result)
}

/// Factory for wifi::Mac
struct WifiMacFactory;

impl BlockFactory for WifiMacFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let src_addr = config.parameters.iter()
            .find(|p| p.name == "src_addr")
            .and_then(|p| p.value.as_str())
            .map(parse_mac_addr)
            .unwrap_or_else(|| Ok([0x42; 6]))?;
        
        let dst_addr = config.parameters.iter()
            .find(|p| p.name == "dst_addr")
            .and_then(|p| p.value.as_str())
            .map(parse_mac_addr)
            .unwrap_or_else(|| Ok([0x23; 6]))?;
        
        let bssid = config.parameters.iter()
            .find(|p| p.name == "bssid")
            .and_then(|p| p.value.as_str())
            .map(parse_mac_addr)
            .unwrap_or_else(|| Ok([0xff; 6]))?;
        
        Ok(fg.add_block(wifi::Mac::new(src_addr, dst_addr, bssid)).into())
    }
}

/// Factory for wifi::Encoder
struct WifiEncoderFactory;

impl BlockFactory for WifiEncoderFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let mcs_str = config.parameters.iter()
            .find(|p| p.name == "mcs")
            .and_then(|p| p.value.as_str())
            .unwrap_or("Qpsk_1_2");
        
        let mcs = match mcs_str {
            "Bpsk_1_2" => wifi::Mcs::Bpsk_1_2,
            "Bpsk_3_4" => wifi::Mcs::Bpsk_3_4,
            "Qpsk_1_2" => wifi::Mcs::Qpsk_1_2,
            "Qpsk_3_4" => wifi::Mcs::Qpsk_3_4,
            "Qam16_1_2" => wifi::Mcs::Qam16_1_2,
            "Qam16_3_4" => wifi::Mcs::Qam16_3_4,
            "Qam64_2_3" => wifi::Mcs::Qam64_2_3,
            "Qam64_3_4" => wifi::Mcs::Qam64_3_4,
            _ => wifi::Mcs::Qpsk_1_2,
        };
        
        let encoder: wifi::Encoder = wifi::Encoder::new(mcs);
        Ok(fg.add_block(encoder).into())
    }
}

/// Factory for wifi::Mapper
struct WifiMapperFactory;

impl BlockFactory for WifiMapperFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        let mapper: wifi::Mapper = wifi::Mapper::new();
        Ok(fg.add_block(mapper).into())
    }
}

/// Factory for wifi::Prefix
struct WifiPrefixFactory;

impl BlockFactory for WifiPrefixFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let pad_front = get_param_u32(&config.parameters, "pad_front")? as usize;
        let pad_tail = get_param_u32(&config.parameters, "pad_tail")? as usize;
        
        let prefix: wifi::Prefix = wifi::Prefix::new(pad_front, pad_tail);
        Ok(fg.add_block(prefix).into())
    }
}

/// Factory for wifi::MovingAverage
struct WifiMovingAverageFactory;

impl BlockFactory for WifiMovingAverageFactory {
    fn create(&self, fg: &mut Flowgraph, config: &BlockConfig) -> Result<BlockId> {
        let length = get_param_u32(&config.parameters, "length")? as usize;
        let dtype = config.dtype.as_deref().unwrap_or("f32");
        
        match dtype {
            "Complex32" => {
                let avg: wifi::MovingAverage<Complex32> = wifi::MovingAverage::new(length);
                Ok(fg.add_block(avg).into())
            }
            "f32" | _ => {
                let avg: wifi::MovingAverage<f32> = wifi::MovingAverage::new(length);
                Ok(fg.add_block(avg).into())
            }
        }
    }
}

/// Factory for wifi::SyncShort
struct WifiSyncShortFactory;

impl BlockFactory for WifiSyncShortFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        let sync: wifi::SyncShort = wifi::SyncShort::new();
        Ok(fg.add_block(sync).into())
    }
}

/// Factory for wifi::SyncLong
struct WifiSyncLongFactory;

impl BlockFactory for WifiSyncLongFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        let sync: wifi::SyncLong = wifi::SyncLong::new();
        Ok(fg.add_block(sync).into())
    }
}

/// Factory for wifi::FrameEqualizer
struct WifiFrameEqualizerFactory;

impl BlockFactory for WifiFrameEqualizerFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        let eq: wifi::FrameEqualizer = wifi::FrameEqualizer::new();
        Ok(fg.add_block(eq).into())
    }
}

/// Factory for wifi::Decoder
struct WifiDecoderFactory;

impl BlockFactory for WifiDecoderFactory {
    fn create(&self, fg: &mut Flowgraph, _config: &BlockConfig) -> Result<BlockId> {
        let decoder: wifi::Decoder = wifi::Decoder::new();
        Ok(fg.add_block(decoder).into())
    }
}
