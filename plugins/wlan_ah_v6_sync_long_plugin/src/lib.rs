// Thin plugin wrapper over `wlan_ah::v6::SyncLong`.
//
// Correlates against the S1G LTF derived by inverse DFT from `LTF_FREQ`, takes
// the two strongest peaks (one symbol apart) for the fine CFO, then strips
// cyclic prefixes at the 160-sample S1G stride.

use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::v6::SyncLong;

plugin_api::export_plugin! {
    name: "WlanAhV6SyncLong",
    description: "802.11ah v6 long preamble synchronization (11a fork)",
    config: (),
    create: |_cfg, _id| {
        SyncLong::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new()
    }
}
