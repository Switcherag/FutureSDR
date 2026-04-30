use clap::Parser;
use futuresdr::async_io::block_on;
use futuresdr::blocks::Apply;
use futuresdr::blocks::FileSource;
use futuresdr::blocks::MessagePipe;
use futuresdr::prelude::*;
use std::path::{Path, PathBuf};

use wlan_ah::Decoder;
use wlan_ah::v2::{
    CfoCorrector, ChannelEstimator, DataDemod, SigDecoder, StfDetector, StoCorrector,
};

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    #[clap(short, long, default_value_t = false)]
    dc_offset: bool,
    #[clap(
        short = 'i',
        long,
        default_value = "bin/2026-04-27-15-22-31_wlan_ah_905M_4Msps_10s.cf32"
    )]
    file: String,
    #[clap(short, long, default_value_t = 1e9)]
    rate: f64,
    #[clap(long, default_value_t = false)]
    debug_print: bool,
}

fn resolve_from_manifest_dir(raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let input_path = resolve_from_manifest_dir(&args.file);

    let rt = Runtime::new();
    let mut fg = Flowgraph::new();

    let source = FileSource::<Complex32>::new(&input_path, false);
    let throttle = futuresdr::blocks::Throttle::<Complex32>::new(args.rate);
    let convert = Apply::<_, _, _>::new(|c: &Complex32| *c);
    connect!(fg, source > throttle > convert);
    let convert_id: BlockId = convert.into();

    let (prev, output): (BlockId, &str) = if args.dc_offset {
        let mut avg_real = 0.0f32;
        let mut avg_imag = 0.0f32;
        let ratio = 1.0e-5f32;
        let dc = fg.add_block(Apply::<_, _, _>::new(move |c: &Complex32| -> Complex32 {
            avg_real = ratio * (c.re - avg_real) + avg_real;
            avg_imag = ratio * (c.im - avg_imag) + avg_imag;
            Complex32::new(c.re - avg_real, c.im - avg_imag)
        }));
        let dc_id: BlockId = dc.into();
        fg.connect_dyn(convert_id, "output", dc_id, "input")?;
        (dc_id, "output")
    } else {
        (convert_id, "output")
    };

    let stf_block: StfDetector = StfDetector::new();
    let cfo_block: CfoCorrector = CfoCorrector::new();
    let sto_block: StoCorrector = StoCorrector::new();
    let ch_block: ChannelEstimator = ChannelEstimator::new_with_debug_print(args.debug_print);
    let sig_block: SigDecoder = SigDecoder::new_with_debug_print(args.debug_print);
    let data_block: DataDemod = DataDemod::new_with_debug_print(args.debug_print);
    let decoder: Decoder = Decoder::new();

    let stf = fg.add_block(stf_block);
    let cfo = fg.add_block(cfo_block);
    let sto = fg.add_block(sto_block);
    let ch = fg.add_block(ch_block);
    let sig = fg.add_block(sig_block);
    let data = fg.add_block(data_block);

    let stf_id: BlockId = stf.clone().into();
    fg.connect_dyn(prev, output, stf_id, "input")?;
    connect!(fg,
        stf.frame | frame.cfo;
        cfo.frame | frame.sto;
        sto.frame | frame.ch;
        ch.frame | frame.sig;
        sig.frame | frame.data;
        data > decoder
    );

    let (tx_frame, mut rx_frame) = mpsc::channel::<Pmt>(100);
    let message_pipe = MessagePipe::new(tx_frame);
    let udp1 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55555");
    let udp2 = futuresdr::blocks::BlobToUdp::new("127.0.0.1:55556");
    connect!(fg, decoder.rx_frames | message_pipe;
                 decoder.rx_frames | udp1;
                 decoder.rftap | udp2);

    let (fg_task, _handle) = rt.start_sync(fg)?;
    block_on(async move {
        fg_task.await?;

        loop {
            let x = match rx_frame.try_recv() {
                Ok(x) => x,
                Err(_) => break,
            };
            match x {
                Pmt::Blob(data) => println!("received frame ({} bytes)", data.len()),
                Pmt::Finished => break,
                _ => {}
            }
        }

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}
