// Thin plugin wrapper over `wlan_ah::v6::SyncShort`.
//
// v6 forks 11a's detector and doubles every window for S1G, so the flow must
// feed it a Tu/4 = 32 sample delay, a 96-sample correlation average and a
// 128-sample power average (see `wlan_ah::v6::{STF_DELAY, STF_CORR_WIN,
// STF_POWER_WIN}`). Wiring it with the top-level wlan_ah sizing will not lock.

use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::v6::SyncShort;

plugin_api::export_plugin! {
    name: "WlanAhV6SyncShort",
    description: "802.11ah v6 short preamble synchronization (11a fork)",
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
