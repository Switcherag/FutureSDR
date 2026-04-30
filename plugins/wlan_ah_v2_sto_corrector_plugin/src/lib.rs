use wlan_ah::v2::StoCorrector;

plugin_api::export_plugin! {
    name: "WlanAhV2StoCorrector",
    description: "802.11ah v2 STO corrector",
    config: (),
    create: |_cfg, _id| {
        StoCorrector::new()
    }
}