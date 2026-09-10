// Thin plugin wrapper over `wlan_ah::v6::FrameEqualizer`.
//
// 11a's Sync1/Sync2/Signal/Copy state machine carried to S1G: 52 data
// subcarriers in a 128-point FFT, and a two-symbol SIG-A validated by CRC-4
// in place of the one-symbol SIGNAL field and its parity bit.

use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::v6::FrameEqualizer;

plugin_api::export_plugin! {
    name: "WlanAhV6FrameEqualizer",
    description: "802.11ah v6 frame equalizer + SIG-A decoder (11a fork)",
    config: (),
    create: |_cfg, _id| {
        FrameEqualizer::<DefaultCpuReader<Complex32>, DefaultCpuWriter<u8>>::new()
    }
}
