// Thin plugin wrapper over `lora::FrameSync`.
//
// Config layout: (bandwidth_hz, sf, oversampling, sync_word)
//   bandwidth_hz: 62500 / 125000 / 250000 / 500000
//   sf:           5..=12 (spreading factor)
//   oversampling: typically 4
//   sync_word:    e.g. 0x12 (private) or 0x34 (public)
//
// Header mode is fixed to Explicit; preamble length / startup_timestamp
// use FrameSync defaults; net-id-caching policy is "header_crc_ok".

use futuresdr::prelude::*;
use lora::FrameSync;
use lora::utils::Bandwidth;
use lora::utils::Channel;
use lora::utils::SpreadingFactor;

plugin_api::export_plugin! {
    name: "LoraFrameSync",
    description: "LoRa frame synchronization (preamble detect, CFO/STO, sync word)",
    config: (u32, u8, u8, u8),
    create: |cfg, _id| {
        let (bandwidth_hz, sf_u8, oversampling, sync_word) = cfg;
        let bandwidth = Bandwidth::try_from(bandwidth_hz)
            .expect("invalid bandwidth_hz; expected 62500/125000/250000/500000");
        let sf = SpreadingFactor::try_from(sf_u8)
            .expect("invalid spreading factor; expected 5..=12");
        FrameSync::<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>::new(
            Channel::from(0u32),                  // center freq is set by the radio head
            bandwidth,
            sf,
            false,                                // explicit header
            vec![vec![sync_word as usize]],
            oversampling as usize,
            None,                                 // default preamble length
            Some("header_crc_ok"),
            false,                                // collect_receive_statistics
            None,                                 // startup_timestamp
        )
    }
}
