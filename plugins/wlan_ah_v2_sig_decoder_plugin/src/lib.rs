use wlan_ah::v2::SigDecoder;

plugin_api::export_plugin! {
    name: "WlanAhV2SigDecoder",
    description: "802.11ah v2 SIG decoder",
    config: bool,
    create: |debug_print, _id| {
        SigDecoder::new_with_debug_print(debug_print)
    }
}