// Thin plugin wrapper over `lora::Decoder` (CRC, dewhitening, payload assembly).

use lora::Decoder;

plugin_api::export_plugin! {
    name: "LoraDecoder",
    description: "LoRa payload decoder (CRC, dewhitening, RFTap)",
    config: (),
    create: |_cfg, _id| { Decoder::new() }
}
