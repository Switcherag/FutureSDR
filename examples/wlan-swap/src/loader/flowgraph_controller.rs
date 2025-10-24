//! Flowgraph Controller Block
//! 
//! A block that listens for control messages to switch flowgraphs.
//! Also acts as a proxy for MAC tx/rx messages.

use futuresdr::prelude::*;
use std::sync::{Mutex, OnceLock};
use std::sync::mpsc;

/// Global reload channel for flowgraph switching
static RELOAD_CHANNEL: OnceLock<Mutex<mpsc::Sender<String>>> = OnceLock::new();

/// Set the reload channel (called once at startup)
pub fn set_reload_channel(tx: mpsc::Sender<String>) {
    RELOAD_CHANNEL.set(Mutex::new(tx)).ok();
}

/// Block that receives PMT commands to switch flowgraphs and proxies MAC messages
/// - Port "control": Receives Pmt::String messages with flowgraph paths
/// - Port "tx": Forwards messages to MAC block (for transmission)
/// - Port "rx": Receives messages from MAC block (for reception)
/// - Port "tx_out": Forwards TX messages to MAC
/// - Port "rx_out": Forwards RX messages to WebSocket sink
#[derive(Block)]
#[message_inputs(control, tx, rx)]
#[message_outputs(tx_out, rx_out)]
pub struct FlowgraphController {}

impl FlowgraphController {
    pub fn new() -> Self {
        FlowgraphController {}
    }

    async fn control(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::String(path) => {
                info!("FlowgraphController: Received reload request for {}", path);
                
                // Send reload signal through global channel
                if let Some(tx_mutex) = RELOAD_CHANNEL.get() {
                    if let Ok(tx) = tx_mutex.lock() {
                        match tx.send(path.clone()) {
                            Ok(_) => {
                                info!("FlowgraphController: Reload signal sent successfully");
                                Ok(Pmt::Ok)
                            }
                            Err(e) => {
                                error!("FlowgraphController: Failed to send reload signal: {}", e);
                                Ok(Pmt::String(format!("Error: {}", e)))
                            }
                        }
                    } else {
                        error!("FlowgraphController: Failed to lock reload channel");
                        Ok(Pmt::String("Error: Channel lock failed".to_string()))
                    }
                } else {
                    warn!("FlowgraphController: No reload channel configured");
                    Ok(Pmt::String("Error: No reload channel".to_string()))
                }
            }
            _ => {
                warn!("FlowgraphController: Expected Pmt::String, got {:?}", p);
                Ok(Pmt::String("Error: Expected Pmt::String".to_string()))
            }
        }
    }

    async fn tx(
        &mut self,
        _io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        // Forward TX message to MAC block
        info!("FlowgraphController: Received TX message: {:?}", p);
        match mio.post("tx_out", p.clone()).await {
            Ok(_) => {
                info!("FlowgraphController: TX message forwarded to MAC successfully");
                Ok(Pmt::Ok)
            }
            Err(e) => {
                error!("FlowgraphController: Failed to forward TX message: {:?}", e);
                Ok(Pmt::String(format!("Error forwarding: {:?}", e)))
            }
        }
    }

    async fn rx(
        &mut self,
        _io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        // Convert Blob to String for GUI display
        let display_msg = match p {
            Pmt::Blob(bytes) => {
                match String::from_utf8(bytes.clone()) {
                    Ok(s) => {
                        println!("\n=== Frame Received! ===");
                        println!("Message: {}", s);
                        println!("======================\n");
                        Pmt::String(s)
                    }
                    Err(_) => {
                        println!("\n=== Frame Received! err ===");
                        println!("Raw bytes: {:?}", bytes);
                        println!("======================\n");
                        // byte to str
                        println!("{}", ToString::to_string(&String::from_utf8_lossy(&bytes)));
                        println!("{}", String::from_utf8_lossy(&bytes));
                        Pmt::String(format!("{:?}", String::from_utf8_lossy(&bytes)))
                    }
                }
            }
            Pmt::String(s) => {
                println!("\n=== Frame Received! ===");
                println!("Message: {}", s);
                println!("======================\n");
                Pmt::String(s)
            }
            _ => {
                println!("\n=== Frame Received! ===");
                println!("Message: {:?}", p);
                println!("======================\n");
                Pmt::String(format!("{:?}", p))
            }
        };

        // Forward the converted message to rx_out (WebSocketPmtSink)
        info!("FlowgraphController: Forwarding RX message to WebSocket");
        match mio.post("rx_out", display_msg).await {
            Ok(_) => {
                info!("FlowgraphController: RX message forwarded to WebSocket successfully");
                Ok(Pmt::Ok)
            }
            Err(e) => {
                error!("FlowgraphController: Failed to forward RX message: {:?}", e);
                Ok(Pmt::String(format!("Error forwarding: {:?}", e)))
            }
        }
    }
}

impl Kernel for FlowgraphController {}
