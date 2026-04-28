// Thin plugin wrapper over `wlan_ah::FrameEqualizer`.

use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::FrameEqualizer;

plugin_api::export_plugin! {
    name: "WlanAhFrameEqualizer",
    description: "802.11ah OFDM frame equalizer + SIG decoder",
    config: (),
    create: |_cfg, _id| {
        FrameEqualizer::<DefaultCpuReader<Complex32>, DefaultCpuWriter<u8>>::new()
    }
}
