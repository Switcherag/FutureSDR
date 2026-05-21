// Thin plugin wrapper over `lora::GrayMapping` (soft-decoding variant).
// In soft mode the block is effectively a passthrough — gray mapping is
// folded into FftDemod. Kept as a discrete block to mirror the upstream
// pipeline shape.

use futuresdr::prelude::*;
use lora::GrayMapping;
use lora::utils::DemodulatedSymbolSoftDecoding;

plugin_api::export_plugin! {
    name: "LoraGrayMapping",
    description: "LoRa Gray code mapping (soft-decoding passthrough)",
    config: (),
    create: |_cfg, _id| {
        GrayMapping::<
            DemodulatedSymbolSoftDecoding,
            DefaultCpuReader<DemodulatedSymbolSoftDecoding>,
            DefaultCpuWriter<DemodulatedSymbolSoftDecoding>,
        >::new()
    }
}
