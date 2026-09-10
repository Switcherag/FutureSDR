//! Monolithic 802.11ah v6 receiver — the non-plugin twin of
//! `examples/real_device_swap/flows/halowv6A.toml`.
//!
//!   SeifySource(4 MSps) ─┬─ Delay(32) ────────────────┬─ SyncShort ─ SyncLong
//!                        ├─ |x|^2 ─ MA(128) ──┐       │
//!                        └─ x*conj(xd) ─ MA(96) ┴ div ┘
//!                        ... ─ Fft(128) ─ FrameEqualizer ─ Decoder ─ BlobToUdp
//!
//! Same blocks and same window sizes as the TOML flow (STF_DELAY 32,
//! STF_CORR_WIN 96, STF_POWER_WIN 128, FFT_SIZE 128), linked in statically.

use anyhow::Result;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::{Apply, BlobToUdp, Combine, Delay, Fft};
use futuresdr::macros::connect;
use futuresdr::prelude::*;
use wlan_ah::v6::{
    FrameEqualizer, STF_CORR_WIN, STF_DELAY, STF_POWER_WIN, SyncLong, SyncShort,
};
use wlan_ah::{Decoder, FFT_SIZE};

fn main() -> Result<()> {
    futuresdr::runtime::init();
    let mut fg = Flowgraph::new();

    let src = fg.add_block(
        Builder::new("")?
            .frequency(919.0e6)
            .sample_rate(4e6)
            .gain(0.0)
            .build_source()?,
    );
    let src_id: BlockId = src.into();

    let delay = fg.add_block(Delay::<Complex32>::new(STF_DELAY as isize));
    fg.connect_dyn(src_id, "outputs[0]", &delay, "input")?;

    let c2m = fg.add_block(Apply::<_, _, _>::new(|i: &Complex32| i.norm_sqr()));
    let float_avg = wlan_ah::MovingAverage::<f32>::new(STF_POWER_WIN);
    fg.connect_dyn(src_id, "outputs[0]", &c2m, "input")?;
    connect!(fg, c2m > float_avg);

    let mult_conj = fg.add_block(Combine::<_, _, _, _>::new(
        |a: &Complex32, b: &Complex32| a * b.conj(),
    ));
    let complex_avg = wlan_ah::MovingAverage::<Complex32>::new(STF_CORR_WIN);
    fg.connect_dyn(src_id, "outputs[0]", &mult_conj, "in0")?;
    connect!(fg, mult_conj > complex_avg;
                 delay > in1.mult_conj);

    let divide_mag = fg.add_block(Combine::<_, _, _, _>::new(|a: &Complex32, b: &f32| {
        a.norm() / b
    }));
    connect!(fg, complex_avg > in0.divide_mag; float_avg > in1.divide_mag);
    let divide_mag_id: BlockId = divide_mag.into();

    let sync_short: SyncShort = SyncShort::new();
    connect!(fg, delay > in_sig.sync_short;
                 complex_avg > in_abs.sync_short);
    fg.connect_dyn(divide_mag_id, "output", &sync_short, "in_cor")?;

    let sync_long: SyncLong = SyncLong::new();
    let fft: Fft = Fft::new(FFT_SIZE);
    let frame_eq: FrameEqualizer = FrameEqualizer::new();
    let decoder = Decoder::new();
    let udp = BlobToUdp::new("127.0.0.1:55555");

    connect!(fg, sync_short > sync_long > fft > frame_eq > decoder;
                 decoder.rx_frames | udp);

    Runtime::new().run(fg)?;
    Ok(())
}
