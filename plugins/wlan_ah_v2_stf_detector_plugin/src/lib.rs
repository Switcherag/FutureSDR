use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::v2::StfDetector;

plugin_api::export_plugin! {
    name: "WlanAhV2StfDetector",
    description: "802.11ah v2 STF detector",
    config: (),
    create: |_cfg, _id| {
        StfDetector::<DefaultCpuReader<Complex32>>::new()
    }
}