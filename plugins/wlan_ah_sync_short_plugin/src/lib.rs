// Thin plugin wrapper over `wlan_ah::SyncShort`.

use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::SyncShort;

plugin_api::export_plugin! {
    name: "WlanAhSyncShort",
    description: "802.11ah short preamble synchronization",
    config: (),
    create: |_cfg, _id| {
        SyncShort::<
            DefaultCpuReader<Complex32>,
            DefaultCpuReader<Complex32>,
            DefaultCpuReader<f32>,
            DefaultCpuWriter<Complex32>,
        >::new()
    }
}
