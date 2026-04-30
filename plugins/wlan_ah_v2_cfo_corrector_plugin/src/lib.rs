use wlan_ah::v2::CfoCorrector;

plugin_api::export_plugin! {
    name: "WlanAhV2CfoCorrector",
    description: "802.11ah v2 CFO corrector",
    config: (),
    create: |_cfg, _id| {
        CfoCorrector::new()
    }
}