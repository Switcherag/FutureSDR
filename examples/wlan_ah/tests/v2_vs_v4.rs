//! v4 is v2 with the frame clones and the per-`work()` input copy removed.
//! The DSP is untouched, so both pipelines must decode a capture identically.

use futuresdr::blocks::FileSource;
use futuresdr::blocks::MessagePipe;
use futuresdr::prelude::*;
use wlan_ah::Decoder;

/// Build and run the six-stage pipeline from module `$ver`, collecting every
/// PSDU the `Decoder` emits.
macro_rules! run_pipeline {
    ($ver:ident, $path:expr, $max_psdu:expr) => {{
        use wlan_ah::$ver::{
            ChannelEstimator, CfoCorrector, DataDemod, SigDecoder, StfDetector, StoCorrector,
        };

        let rt = Runtime::new();
        let mut fg = Flowgraph::new();

        let src = FileSource::<Complex32>::new($path, false);
        let stf: StfDetector = StfDetector::with_max_psdu($max_psdu);
        let cfo = CfoCorrector::new();
        let sto = StoCorrector::new();
        let ch = ChannelEstimator::new();
        let sig = SigDecoder::new();
        let data: DataDemod = DataDemod::new();
        let decoder = Decoder::new();

        let (tx, mut rx) = mpsc::channel::<Pmt>(1000);
        let pipe = MessagePipe::new(tx);

        connect!(fg,
            src > stf;
            stf.frame | frame.cfo;
            cfo.frame | frame.sto;
            sto.frame | frame.ch;
            ch.frame | frame.sig;
            sig.frame | frame.data;
            data > decoder;
            decoder.rx_frames | pipe
        );

        let (_fg, _handle) = rt.start_sync(fg).unwrap();
        rt.block_on(async move {
            let mut frames: Vec<Vec<u8>> = Vec::new();
            while let Some(p) = rx.next().await {
                match p {
                    Pmt::Blob(d) => frames.push(d),
                    _ => break,
                }
            }
            frames
        })
    }};
}

/// The captures are ~60k samples, well under the 76,448-sample cold start a
/// 1500-byte `StfDetector::new()` needs, so size the detector to the traffic.
const MAX_PSDU: usize = 256;

const CAPTURE_15DB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/bpsk-1-2-15db.cf32");
const CAPTURE_30DB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/bpsk-3-4-30db.cf32");

fn compare(capture: &str) -> Result<usize> {
    let v2_frames = run_pipeline!(v2, capture, MAX_PSDU);
    let v4_frames = run_pipeline!(v4, capture, MAX_PSDU);

    assert_eq!(
        v2_frames.len(),
        v4_frames.len(),
        "{capture}: frame count differs: v2={} v4={}",
        v2_frames.len(),
        v4_frames.len()
    );
    assert_eq!(v2_frames, v4_frames, "{capture}: decoded payloads differ");
    Ok(v2_frames.len())
}

#[test]
fn v4_decodes_identically_to_v2() -> Result<()> {
    let n15 = compare(CAPTURE_15DB)?;
    let n30 = compare(CAPTURE_30DB)?;
    println!("bpsk-1-2-15db: {n15} frames; bpsk-3-4-30db: {n30} frames");
    assert!(
        n15 + n30 > 0,
        "neither capture decoded a frame in either pipeline — the comparison is vacuous"
    );
    Ok(())
}
