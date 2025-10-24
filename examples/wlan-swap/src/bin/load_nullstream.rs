use anyhow::Result;
use clap::Parser;
use futuresdr::prelude::*;
use futuresdr::runtime;
use wlan::loader::load_flowgraph;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    #[clap(default_value = "flowgraphs/nullstream.toml")]
    file: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Loading flowgraph from: {}", args.file);

    let fg = load_flowgraph(&args.file)?;
    println!("Flowgraph loaded successfully!");
    
    println!("Starting runtime...");
    runtime::init();
    Runtime::new().run(fg)?;

    Ok(())
}
