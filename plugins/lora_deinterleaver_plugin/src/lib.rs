// Thin plugin wrapper over `lora::Deinterleaver` (soft-decoding variant).
// Config: spreading factor (u8). LDRO is derived as `sf >= SF11`.

use futuresdr::prelude::*;
use lora::Deinterleaver;
use lora::default_values::ldro;
use lora::utils::DeinterleavedSymbolSoftDecoding;
use lora::utils::DemodulatedSymbolSoftDecoding;
use lora::utils::SpreadingFactor;

plugin_api::export_plugin! {
    name: "LoraDeinterleaver",
    description: "LoRa deinterleaver (soft decoding)",
    config: u8,
    create: |cfg, _id| {
        let sf = SpreadingFactor::try_from(cfg)
            .expect("invalid spreading factor; expected 5..=12");
        Deinterleaver::<
            DemodulatedSymbolSoftDecoding,
            DeinterleavedSymbolSoftDecoding,
            DefaultCpuReader<DemodulatedSymbolSoftDecoding>,
            DefaultCpuWriter<DeinterleavedSymbolSoftDecoding>,
        >::new(ldro(sf), sf)
    }
}
