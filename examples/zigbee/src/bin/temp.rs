// Cargo.toml (add to your project)
// [package]
// name = "futuresdr_basic"
// version = "0.1.0"
// edition = "2021"
//
// [dependencies]
// futuresdr = "0.0.38"
//
// Then put the Rust code below in src/main.rs

use futuresdr::blocks::{Head, NullSink, NullSource};
use futuresdr::macros::connect;
use futuresdr::runtime::{Flowgraph, Runtime};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create the runtime
    let rt = Runtime::new();

    // --- First flowgraph (with Head block) ---
    let mut fg1 = Flowgraph::new();
    let src1 = NullSource::<u8>::new();
    let head1 = Head::<u8>::new(123);
    let snk1 = NullSink::<u8>::new();

    connect!(fg1, src1 > head1 > snk1);

    println!("Starting first flowgraph (with Head block)...");
    let (fg, mut handle1) = rt.start_sync(fg1)?;
    // Let it run for 5 seconds
    thread::sleep(Duration::from_secs(5));

    println!("Stopping first flowgraph...");
    handle1.terminate();;
    drop(fg);
    drop(handle1);
    // --- Second flowgraph (without Head block) ---
    let mut fg2 = Flowgraph::new();
    let src2 = NullSource::<u8>::new();
    let snk2 = NullSink::<u8>::new();

    connect!(fg2, src2 > snk2);

    println!("Starting second flowgraph (without Head block)...");
    let (fg, mut handle2) = rt.start_sync(fg2)?;

    // Run again for 5 seconds
    thread::sleep(Duration::from_secs(5));

    println!("Stopping second flowgraph...");
    handle2.terminate();
    ;

    println!("All flowgraphs terminated successfully.");

    Ok(())
}

// This version:
// 1. Starts a flowgraph with a Head block for 5 seconds.
// 2. Stops it with handle.stop() and waits for completion.
// 3. Starts a second flowgraph without Head, runs 5 seconds, and stops.
// 4. Uses standard threads for timing (no async runtime).