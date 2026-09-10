//! Monolithic dual-PHY receiver — both chains of `flows/zigbee_rxA.toml` and
//! `flows/halowv6A.toml` in one statically linked binary, fed from one source.
//!
//! This is the size the plugin system is really competing with: a monolith that
//! *can* do both PHYs has to carry both, whereas the plugin host loads one PHY's
//! .so set at a time.

use anyhow::Result;
use futuresdr::blocks::seify::Builder;
use futuresdr::blocks::{Apply, BlobToUdp, Combine, Delay, Fft, NullSink};
use futuresdr::macros::connect;
use futuresdr::prelude::*;
use wlan_ah::v6::{
    FrameEqualizer, STF_CORR_WIN, STF_DELAY, STF_POWER_WIN, SyncLong, SyncShort,
};
use wlan_ah::FFT_SIZE;
use zigbee::{ClockRecoveryMm, Mac};

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

    // ── ZigBee branch ────────────────────────────────────────────────
    let mut last: Complex32 = Complex32::new(0.0, 0.0);
    let mut iir: f32 = 0.0;
    let alpha = 0.00016f32;
    let z_demod = fg.add_block(Apply::<_, _, _>::new(move |i: &Complex32| -> f32 {
        let phase = (last.conj() * i).arg();
        last = *i;
        iir = (1.0 - alpha) * iir + alpha * phase;
        phase - iir
    }));
    fg.connect_dyn(src_id, "outputs[0]", &z_demod, "input")?;

    let z_mm: ClockRecoveryMm = ClockRecoveryMm::new(2.0, 0.000225, 0.5, 0.03, 0.0002);
    let z_dec = zigbee::Decoder::new(6);
    let z_mac: Mac = Mac::new();
    let z_snk = NullSink::<u8>::new();
    let z_rftap = BlobToUdp::new("127.0.0.1:55555");
    connect!(fg, z_demod > z_mm > z_dec;
                 z_mac > z_snk;
                 z_dec | rx.z_mac;
                 z_mac.rftap | z_rftap);

    // ── HaLow v6 branch ──────────────────────────────────────────────
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
    let h_dec = wlan_ah::Decoder::new();
    let h_udp = BlobToUdp::new("127.0.0.1:55556");
    connect!(fg, sync_short > sync_long > fft > frame_eq > h_dec;
                 h_dec.rx_frames | h_udp);

    Runtime::new().run(fg)?;
    Ok(())
}
