//! Notebook-faithful all-in-one decoder.
//!
//! Pipeline: FileSource → PpduProcessor → MessagePipe.
//! `PpduProcessor` buffers the entire input, runs notebook-style detection
//! and `decode_ppdu` per detection, and emits each decoded PSDU as a
//! `Pmt::Blob` on its `rx_frames` port.

use clap::Parser;
use futuresdr::blocks::FileSource;
use futuresdr::blocks::MessagePipe;
use futuresdr::prelude::*;

use wlan_ah::v3::PpduProcessor;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Input file path (cf32 = native Complex<f32>, like notebook's `np.fromfile(..., 'complex64')`)
    #[clap(
        short = 'i',
        long,
        default_value = "bin/2026-04-27-15-22-31_wlan_ah_905M_4Msps_10s.cf32"
    )]
    file: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    let source = FileSource::<Complex32>::new(&args.file, false);
    let processor: PpduProcessor = PpduProcessor::new();

    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(1000);
    let pipe = MessagePipe::new(tx_frame);

    connect!(fg, source > processor;
                 processor.rx_frames | pipe);

    let (_fg, _handle) = rt.start_sync(fg)?;
    rt.block_on(async move {
        let mut count = 0usize;
        let mut total_bytes = 0usize;
        while let Some(p) = rx_frame.next().await {
            match p {
                Pmt::Blob(data) => {
                    count += 1;
                    total_bytes += data.len();
                    println!("[v3] frame {} — {} bytes", count, data.len());
                }
                Pmt::Finished => break,
                _ => break,
            }
        }
        println!("[v3] decoded {} frames, {} bytes total", count, total_bytes);
    });

    Ok(())
}
