//! 802.11ah (S1G) channelization — US regulatory domain (902–928 MHz ISM).
//!
//! Source: IEEE 802.11-2020 Annex E.4, Table E-4 (United States).
//! Channel numbering is contiguous across bandwidths: odd-numbered
//! channels are 1 MHz, and wider channels (2/4/8 MHz) occupy even numbers
//! at progressively coarser spacings.

const CHANNELS: [(u32, f64); 48] = [
    // ── 1 MHz (S1G_1M), 26 channels ──────────────────────────────────────
    (1,  902.5e6), (3,  903.5e6), (5,  904.5e6), (7,  905.5e6),
    (9,  906.5e6), (11, 907.5e6), (13, 908.5e6), (15, 909.5e6),
    (17, 910.5e6), (19, 911.5e6), (21, 912.5e6), (23, 913.5e6),
    (25, 914.5e6), (27, 915.5e6), (29, 916.5e6), (31, 917.5e6),
    (33, 918.5e6), (35, 919.5e6), (37, 920.5e6), (39, 921.5e6),
    (41, 922.5e6), (43, 923.5e6), (45, 924.5e6), (47, 925.5e6),
    (49, 926.5e6), (51, 927.5e6),
    // ── 2 MHz (S1G_2M), 13 channels ──────────────────────────────────────
    (2,  903.0e6), (6,  905.0e6), (10, 907.0e6), (14, 909.0e6),
    (18, 911.0e6), (22, 913.0e6), (26, 915.0e6), (30, 917.0e6),
    (34, 919.0e6), (38, 921.0e6), (42, 923.0e6), (46, 925.0e6),
    (50, 927.0e6),
    // ── 4 MHz (S1G_4M), 6 channels ───────────────────────────────────────
    (8,  906.0e6), (16, 910.0e6), (24, 914.0e6),
    (32, 918.0e6), (40, 922.0e6), (48, 926.0e6),
    // ── 8 MHz (S1G_8M), 3 channels ───────────────────────────────────────
    (12, 908.0e6), (28, 916.0e6), (44, 924.0e6),
];

pub fn channel_to_freq(chan: u32) -> Option<f64> {
    CHANNELS
        .iter()
        .find_map(|(c, f)| if chan == *c { Some(*f) } else { None })
}

pub fn parse_channel(s: &str) -> Result<f64, String> {
    let channel: u32 = s
        .parse()
        .map_err(|_| format!("`{s}` isn't an S1G channel number"))?;

    channel_to_freq(channel).ok_or_else(|| format!("`{s}` isn't an S1G channel number"))
}
