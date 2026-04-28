// Thin plugin wrapper over `wlan_ah::SyncLong`.

use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::SyncLong;

plugin_api::export_plugin! {
    name: "WlanAhSyncLong",
    description: "802.11ah long preamble synchronization",
    config: (),
    create: |_cfg, _id| {
        SyncLong::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new()
    }
}
