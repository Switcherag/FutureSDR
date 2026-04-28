// Thin plugin wrapper over `wlan_ah::Decoder`.

use futuresdr::prelude::*;
use wlan_ah::Decoder;

plugin_api::export_plugin! {
    name: "WlanAhDecoder",
    description: "802.11ah Viterbi decoder + MAC frame assembly",
    config: (),
    create: |_cfg, _id| {
        Decoder::<DefaultCpuReader<u8>>::new()
    }
}
