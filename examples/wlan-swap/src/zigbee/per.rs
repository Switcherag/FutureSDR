//! ZigBee Packet Error Rate (PER) Test Block
//!
//! This block generates test traffic for measuring packet error rate at different
//! TX power levels. It sends "loadXXYY" messages where XX is the gain value and YY
//! is the sequence number, allowing the receiver to identify which gain level
//! each packet was transmitted at.
//!
//! The block sweeps TX gain from `gain_start` down to `gain_end` in steps of `gain_step`,
//! sending `packets_per_gain` packets at each level.

use futuresdr::prelude::*;

/// Configuration for the PER test
#[derive(Clone, Debug)]
pub struct PerConfig {
    /// Starting TX gain (dB)
    pub gain_start: f64,
    /// Ending TX gain (dB)
    pub gain_end: f64,
    /// Gain step (will be subtracted)
    pub gain_step: f64,
    /// Number of packets to send at each gain level
    pub packets_per_gain: u32,
    /// Interval between packets in milliseconds
    pub packet_interval_ms: u64,
}

impl Default for PerConfig {
    fn default() -> Self {
        Self {
            gain_start: 88.0,
            gain_end: 0.0,
            gain_step: 4.0,
            packets_per_gain: 1000,
            packet_interval_ms: 10,  // 100 packets/sec
        }
    }
}

/// PER test state
#[derive(Clone, Copy, Debug, PartialEq)]
enum PerState {
    /// Waiting for start command
    Idle,
    /// Running the test
    Running,
    /// Test completed
    Finished,
}

/// ZigBee PER Test Block
///
/// Message inputs:
/// - `ctrl`: Control messages (start/stop)
///
/// Message outputs:
/// - `tx`: Packets to send (connects to MAC's tx port)
/// - `gain`: Gain control messages (connects to seify sink's gain port)
/// - `status`: Status updates
#[derive(Block)]
#[message_inputs(ctrl)]
#[message_outputs(tx, gain, status)]
pub struct Per {
    config: PerConfig,
    state: PerState,
    current_gain: f64,
    packets_sent_at_current_gain: u32,
    total_packets_sent: u64,
}

impl Per {
    /// Create a new PER test block with default configuration
    pub fn new() -> Self {
        Self::with_config(PerConfig::default())
    }

    /// Create a new PER test block with custom configuration
    pub fn with_config(config: PerConfig) -> Self {
        Self {
            current_gain: config.gain_start,
            config,
            state: PerState::Idle,
            packets_sent_at_current_gain: 0,
            total_packets_sent: 0,
        }
    }

    /// Create a new PER test block from individual parameters
    pub fn with_params(
        gain_start: f64,
        gain_end: f64,
        gain_step: f64,
        packets_per_gain: u32,
        packet_interval_ms: u64,
    ) -> Self {
        Self::with_config(PerConfig {
            gain_start,
            gain_end,
            gain_step,
            packets_per_gain,
            packet_interval_ms,
        })
    }

    /// Format the test message: "loadXXYYYY" where XX is gain (2 digits) and YYYY is sequence
    fn format_message(&self, seq: u32) -> Vec<u8> {
        // Format: "loadGGSSSS" where GG is gain (2 chars), SSSS is sequence (4 chars)
        format!("load{:02}{:04}", self.current_gain as u32, seq).into_bytes()
    }

    /// Handle control messages
    async fn ctrl(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::String(ref cmd) if cmd == "start" => {
                if self.state == PerState::Idle {
                    info!("PER test starting: gain {} -> {} (step {}), {} packets/level",
                        self.config.gain_start, self.config.gain_end, 
                        self.config.gain_step, self.config.packets_per_gain);
                    
                    // Reset state
                    self.current_gain = self.config.gain_start;
                    self.packets_sent_at_current_gain = 0;
                    self.total_packets_sent = 0;
                    self.state = PerState::Running;
                    
                    // Set initial gain
                    mio.post("gain", Pmt::F64(self.current_gain)).await?;
                    info!("Set initial TX gain to {} dB", self.current_gain);
                    
                    // Notify status
                    mio.post("status", Pmt::String(format!("started:gain={}", self.current_gain))).await?;
                    
                    // Trigger work to start sending
                    io.notify_work();
                }
            }
            Pmt::String(ref cmd) if cmd == "stop" => {
                if self.state == PerState::Running {
                    info!("PER test stopped by user after {} packets", self.total_packets_sent);
                    self.state = PerState::Idle;
                    mio.post("status", Pmt::String("stopped".into())).await?;
                }
            }
            Pmt::String(ref cmd) if cmd == "status" => {
                let status = format!(
                    "state={:?}, gain={}, packets_at_gain={}/{}, total={}",
                    self.state, self.current_gain, 
                    self.packets_sent_at_current_gain, self.config.packets_per_gain,
                    self.total_packets_sent
                );
                return Ok(Pmt::String(status));
            }
            Pmt::Null => {
                // Auto-start on init signal (from FlowgraphController)
                if self.state == PerState::Idle {
                    info!("PER test auto-starting on init signal");
                    self.current_gain = self.config.gain_start;
                    self.packets_sent_at_current_gain = 0;
                    self.total_packets_sent = 0;
                    self.state = PerState::Running;
                    
                    mio.post("gain", Pmt::F64(self.current_gain)).await?;
                    info!("Set initial TX gain to {} dB", self.current_gain);
                    mio.post("status", Pmt::String(format!("started:gain={}", self.current_gain))).await?;
                    
                    io.notify_work();
                }
            }
            _ => {
                warn!("PER: Unknown control command: {:?}", p);
            }
        }
        Ok(Pmt::Ok)
    }
}

impl Default for Per {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for Per {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        if self.state != PerState::Running {
            return Ok(());
        }

        // Send one packet
        let msg = self.format_message(self.packets_sent_at_current_gain);
        mio.post("tx", Pmt::Blob(msg)).await?;
        
        self.packets_sent_at_current_gain += 1;
        self.total_packets_sent += 1;

        // Check if we need to move to next gain level
        if self.packets_sent_at_current_gain >= self.config.packets_per_gain {
            // Move to next gain level
            self.current_gain -= self.config.gain_step;
            self.packets_sent_at_current_gain = 0;
            
            if self.current_gain < self.config.gain_end {
                // Test complete
                info!("PER test complete! Total packets sent: {}", self.total_packets_sent);
                self.state = PerState::Finished;
                mio.post("status", Pmt::String(format!("finished:total={}", self.total_packets_sent))).await?;
                io.finished = true;
                return Ok(());
            }
            
            // Set new gain
            mio.post("gain", Pmt::F64(self.current_gain)).await?;
            info!("Switched to TX gain {} dB ({} packets sent so far)", 
                self.current_gain, self.total_packets_sent);
            mio.post("status", Pmt::String(format!("gain_changed:gain={}", self.current_gain))).await?;
        }

        // Log progress every 100 packets
        if self.total_packets_sent % 100 == 0 {
            debug!("PER progress: {} packets sent (gain={}, {}/{})", 
                self.total_packets_sent, self.current_gain,
                self.packets_sent_at_current_gain, self.config.packets_per_gain);
        }

        // Schedule next packet after interval
        smol::Timer::after(std::time::Duration::from_millis(self.config.packet_interval_ms)).await;
        
        // Continue sending
        io.notify_work();
        
        Ok(())
    }
}
