//! Monolithic ZigBee 802.15.4 receiver — the non-plugin twin of
//! `examples/real_device_swap/flows/zigbee_rxA.toml`.
//!
//!   SeifySource(4 MSps) -> demod -> ClockRecoveryMm -> Decoder -> Mac -> NullSink
//!
//! Same blocks, same constants as the TOML flow; the only difference is that
//! they are linked in statically instead of being dlopen'd from six .so files.

use anyhow::Result;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::{Apply, BlobToUdp, NullSink};
use futuresdr::macros::connect;
use futuresdr::prelude::*;
use zigbee::{ClockRecoveryMm, Decoder, Mac};

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let mut fg = Flowgraph::new();

    // flows/sdr_head.toml: native 4 MSps, no decimation stage.
    let src = fg.add_block(
        Builder::new("")?
            .frequency(2.425e9)
            .sample_rate(4e6)
            .gain(0.0)
            .build_source()?,
    );

    // zigbee_demod_plugin, alpha = 0.00016
    let mut last: Complex32 = Complex32::new(0.0, 0.0);
    let mut iir: f32 = 0.0;
    let alpha = 0.00016f32;
    let demod = fg.add_block(Apply::<_, _, _>::new(move |i: &Complex32| -> f32 {
        let phase = (last.conj() * i).arg();
        last = *i;
        iir = (1.0 - alpha) * iir + alpha * phase;
        phase - iir
    }));
    fg.connect_dyn(src, "outputs[0]", &demod, "input")?;

    // clock_recovery_mm_plugin config = [2.0, 0.000225, 0.5, 0.03, 0.0002]
    let mm: ClockRecoveryMm = ClockRecoveryMm::new(2.0, 0.000225, 0.5, 0.03, 0.0002);
    let decoder = Decoder::new(6);
    let mac: Mac = Mac::new();
    let snk = NullSink::<u8>::new();
    let rftap = BlobToUdp::new("127.0.0.1:55555");

    connect!(fg, demod > mm > decoder;
                 mac > snk;
                 decoder | rx.mac;
                 mac.rftap | rftap);

    Runtime::new().run(fg)?;
    Ok(())
}
