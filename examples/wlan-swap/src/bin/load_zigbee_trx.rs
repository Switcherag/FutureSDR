use std::time::Duration;

use anyhow::{Result, Context};
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::async_io::block_on;
use futuresdr::prelude::*;
use wlan::loader::load_flowgraph_with_loader;


#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    #[clap(default_value = "flowgraphs/zigbee_trx.toml")]
    file: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Loading flowgraph from: {}", args.file);

    let (fg, loader) = load_flowgraph_with_loader(&args.file)?;
    println!("Flowgraph loaded successfully!");
    
    // Get runtime configuration
    let runtime_config = loader.config().runtime.as_ref()
        .context("No runtime configuration found")?;
    
    // Get the first async task (periodic sender)
    let task = runtime_config.async_tasks.first()
        .context("No async tasks configured")?;
    
    // Get the block ID for the MAC
    let mac_block_id = loader.get_block(&task.block)
        .with_context(|| format!("Block '{}' not found", task.block))?;
    
    let interval = task.interval_secs.unwrap_or(0.06);
    let port = task.port.clone();
    let message_pattern = task.message_pattern.clone();
    
    println!("Starting runtime...");
    let rt = Runtime::new();
    let (fg, mut handle) = rt.start_sync(fg)?;

    // Send periodic messages as configured in TOML
    let mut seq = 0u64;
    rt.spawn_background(async move {
        loop {
            Timer::after(Duration::from_secs_f32(interval)).await;
            let message = message_pattern.replace("{seq}", &seq.to_string());
            handle
                .call(
                    mac_block_id,
                    port.as_str(),
                    Pmt::Blob(message.as_bytes().to_vec()),
                )
                .await
                .unwrap();
            seq += 1;
        }
    });

    block_on(fg)?;

    Ok(())
}