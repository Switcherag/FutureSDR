// WLAN (WiFi) library module
pub mod wifi;

// ZigBee library module
pub mod zigbee;

// TOML-based flowgraph loader
pub mod loader;

// Re-export commonly used WLAN items for convenience
pub use wifi::{
    channel_to_freq, parse_channel, Decoder, Encoder, FrameEqualizer, Mac, Mapper,
    MovingAverage, Prefix, SyncLong, SyncShort, ViterbiDecoder, Modulation,
    MAX_PAYLOAD_SIZE, MAX_PSDU_SIZE, MAX_SYM, MAX_ENCODED_BITS,
    Mcs, FrameParam, LONG, POLARITY,
};

// Re-export commonly used ZigBee items for convenience
pub use zigbee::{
    ClockRecoveryMm, Decoder as ZigbeeDecoder, IqDelay, Mac as ZigbeeMac, modulator,
    channel_to_freq as zigbee_channel_to_freq,
    parse_channel as zigbee_parse_channel,
};

#[cfg(target_arch = "wasm32")]
pub mod wasm;
