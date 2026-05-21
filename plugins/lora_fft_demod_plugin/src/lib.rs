// Thin plugin wrapper over `lora::FftDemod` (soft-decoding variant).
// Config: spreading factor (u8, 5..=12). LDRO is derived as `sf >= SF11`.

use futuresdr::prelude::*;
use lora::FftDemod;
use lora::default_values::ldro;
use lora::utils::DemodulatedSymbolSoftDecoding;
use lora::utils::SpreadingFactor;

plugin_api::export_plugin! {
    name: "LoraFftDemod",
    description: "LoRa FFT-based symbol demodulator (soft decoding)",
    config: u8,
    create: |cfg, _id| {
        let sf = SpreadingFactor::try_from(cfg)
            .expect("invalid spreading factor; expected 5..=12");
        FftDemod::<
            DemodulatedSymbolSoftDecoding,
            _,
            DefaultCpuReader<Complex32>,
            DefaultCpuWriter<DemodulatedSymbolSoftDecoding>,
        >::new(sf, ldro(sf))
    }
}
