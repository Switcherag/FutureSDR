// Thin plugin wrapper over `lora::HeaderDecoder` (Explicit header mode).
// Config: spreading factor (u8). LDRO is derived as `sf >= SF11`.

use futuresdr::prelude::*;
use lora::HeaderDecoder;
use lora::HeaderMode;
use lora::default_values::ldro;
use lora::utils::SpreadingFactor;

plugin_api::export_plugin! {
    name: "LoraHeaderDecoder",
    description: "LoRa header decoder (explicit header mode)",
    config: u8,
    create: |cfg, _id| {
        let sf = SpreadingFactor::try_from(cfg)
            .expect("invalid spreading factor; expected 5..=12");
        HeaderDecoder::<DefaultCpuReader<u8>>::new(HeaderMode::Explicit, ldro(sf))
    }
}
