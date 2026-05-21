// Thin plugin wrapper over `lora::HammingDecoder` (soft-decoding variant).

use futuresdr::prelude::*;
use lora::HammingDecoder;
use lora::utils::DeinterleavedSymbolSoftDecoding;

plugin_api::export_plugin! {
    name: "LoraHammingDecoder",
    description: "LoRa Hamming FEC decoder (soft decoding)",
    config: (),
    create: |_cfg, _id| {
        HammingDecoder::<
            DeinterleavedSymbolSoftDecoding,
            DefaultCpuReader<DeinterleavedSymbolSoftDecoding>,
            DefaultCpuWriter<u8>,
        >::new()
    }
}
