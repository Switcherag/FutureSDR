use clap::Parser;
use anyhow::Result;
use futuresdr::runtime::{Runtime, FlowgraphHandle};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use wlan::loader::{
    load_flowgraph_with_loader,
    write_control_file,
};

#[derive(Parser, Debug)]
#[clap(author, version, about = "FutureSDR Radio Frontend - Switchable WiFi/ZigBee TX/RX")]
struct Args {
    /// Flowgraph configuration file to load
    #[clap(short, long)]
    file: Option<String>,

    /// Mode: wifi_tx, wifi_rx, zigbee_tx, zigbee_rx
    #[clap(short, long)]
    mode: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Determine initial file from mode or file argument
    let initial_file = if let Some(ref mode) = args.mode {
        format!("flowgraphs/{}.toml", mode)
    } else if let Some(ref file) = args.file {
        file.clone()
    } else {
        // Use zigbee_trx flowgraph by default for testing
        "flowgraphs/zigbee_trx.toml".to_string()
    };
    
    // Write to control file for initial state
    write_control_file(&initial_file)?;
    println!("=== FutureSDR Radio Frontend ===");
    println!("Initial flowgraph: {}", initial_file);
    println!("Hot-reload: Web GUI can switch flowgraphs via control message");
    println!();
    
    // Create channel for reload signals
    let (reload_tx, reload_rx) = mpsc::channel::<String>();
    
    // Set the global reload channel for FlowgraphController
    wlan::loader::flowgraph_controller::set_reload_channel(reload_tx);
    
    // Create Runtime once
    let rt = Runtime::new();
    println!(">>> Runtime started at http://127.0.0.1:1337");
    
    // Spawn dedicated listener thread that owns the flowgraph handle
    thread::spawn(move || {
        use futuresdr::async_io::block_on;
        let mut current_file = initial_file;
        let mut fg_handle_opt: Option<FlowgraphHandle> = None;
        
        loop {
            println!("\n>>> Loading flowgraph: {}", current_file);
            
            // First, terminate the old flowgraph if it exists
            if let Some(mut old_handle) = fg_handle_opt.take() {
                println!(">>> Terminating old flowgraph...");
                block_on(async {
                    if let Err(e) = old_handle.terminate_and_wait().await {
                        eprintln!("Error during old flowgraph termination: {}", e);
                    }
                });
                println!(">>> Old flowgraph fully terminated");
            }
            
            // Now load and start the new flowgraph
            match load_flowgraph_with_loader(&current_file) {
                Ok((fg, loader)) => {
                    println!(">>> Flowgraph loaded successfully!");
                    
                    // Debug: print controller block ID
                    if let Some(controller_id) = loader.get_block("flowgraph_controller") {
                        println!(">>> FlowgraphController is at block ID: {:?}", controller_id);
                    } else {
                        println!(">>> WARNING: FlowgraphController not found in block_map!");
                    }
                    
                    // Start new flowgraph
                    let (_fg_task, mut new_fg_handle) = match rt.start_sync(fg) {
                        Ok(h) => h,
                        Err(e) => {
                            eprintln!(">>> Failed to start flowgraph: {}", e);
                            thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                    };
                    
                    println!(">>> Flowgraph running. Listening for reload signals...");
                    
                    // Send reload message to FlowgraphController RX port so frontend can reload
                    if let Some(controller_id) = loader.get_block("flowgraph_controller") {
                        use futuresdr::runtime::Pmt;
                        let _ = new_fg_handle.call(controller_id, "rx", Pmt::String("reload".to_string()));
                        println!(">>> Sent reload notification to FlowgraphController RX port");
                    }

                    // Keep the new handle for next iteration
                    fg_handle_opt = Some(new_fg_handle);
                    
                    // Wait for reload signal from channel
                    match reload_rx.recv_timeout(Duration::from_secs(3600)) {
                        Ok(new_file) => {
                            println!("\n>>> Reload signal received!");
                            println!(">>> Switching from {} to {}", current_file, new_file);
                            
                            // Update to new flowgraph file and loop will handle termination + reload
                            current_file = new_file;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            // Continue running - just checking channel periodically
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            println!(">>> Reload channel disconnected, exiting...");
                            break;
                        }
                    }
                }
                Err(e) => {
                    eprintln!(">>> Failed to load flowgraph: {}", e);
                    eprintln!(">>> Retrying in 2 seconds...");
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    });
    
    // Main thread keeps runtime alive
    println!(">>> Press Ctrl+C to stop");
    loop {
        thread::sleep(Duration::from_secs(160));
    }
}
