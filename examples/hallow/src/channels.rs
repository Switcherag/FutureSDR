/// HaLow S1G channel to frequency mapping (US, IEEE 802.11ah-2016)
/// Sub-1 GHz band: 902.5-927.5 MHz
pub fn channel_to_freq(channel: u32) -> Option<f64> {
    // 1 MHz channels
    let freq = match channel {
        1 => 902.5e6,
        3 => 903.5e6,
        5 => 904.5e6,
        7 => 905.5e6,
        9 => 906.5e6,
        11 => 907.5e6,
        36 => 908.5e6,
        37 => 909.5e6,
        38 => 910.5e6,
        39 => 911.5e6,
        40 => 912.5e6,
        41 => 913.5e6,
        42 => 914.5e6,
        43 => 915.5e6,
        44 => 916.5e6,
        45 => 917.5e6,
        46 => 918.5e6,
        47 => 919.5e6,
        48 => 920.5e6,
        100 => 925.5e6,
        104 => 926.5e6,
        108 => 927.5e6,
        149 => 921.5e6,
        150 => 922.5e6,
        151 => 923.5e6,
        152 => 924.5e6,
        // 2 MHz channels
        2 => 903.0e6,
        6 => 905.0e6,
        10 => 907.0e6,
        112 => 927.0e6,
        153 => 909.0e6,
        154 => 911.0e6,
        155 => 913.0e6,
        156 => 915.0e6,
        157 => 917.0e6,
        158 => 919.0e6,
        159 => 921.0e6,
        160 => 923.0e6,
        161 => 925.0e6,
        // 4 MHz channels
        8 => 906.0e6,
        116 => 926.0e6,
        162 => 910.0e6,
        163 => 914.0e6,
        164 => 918.0e6,
        165 => 922.0e6,
        _ => return None,
    };
    Some(freq)
}

pub fn parse_channel(s: &str) -> Result<f64, String> {
    let channel: u32 = s.parse().map_err(|_| format!("Invalid channel number: {s}"))?;
    channel_to_freq(channel).ok_or_else(|| format!("Unknown HaLow channel: {channel}"))
}
