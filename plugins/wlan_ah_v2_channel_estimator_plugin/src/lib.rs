use wlan_ah::v2::ChannelEstimator;

plugin_api::export_plugin! {
    name: "WlanAhV2ChannelEstimator",
    description: "802.11ah v2 channel estimator",
    config: bool,
    create: |debug_print, _id| {
        ChannelEstimator::new_with_debug_print(debug_print)
    }
}