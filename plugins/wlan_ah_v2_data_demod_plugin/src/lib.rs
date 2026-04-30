use futuresdr::prelude::*;
use wlan_ah::v2::DataDemod;

plugin_api::export_plugin! {
    name: "WlanAhV2DataDemod",
    description: "802.11ah v2 data demodulator",
    config: bool,
    create: |debug_print, _id| {
        DataDemod::<DefaultCpuWriter<u8>>::new_with_debug_print(debug_print)
    }
}