//! Reference sequences of IEEE 802.11-2020, from `examples/wlan` (802.11a)
//! and the `dyn` branch's `wlan_ah` (802.11ah, S1G 2 MHz).

use futuresdr::num_complex::Complex32;

/// Pilot polarity p_n (17.3.5.10), cyclic over 127 symbols.
pub const POLARITY: [i8; 127] = [
    1, 1, 1, 1, -1, -1, -1, 1, -1, -1, -1, -1, 1, 1, -1, 1, -1, -1, 1, 1, -1, 1, 1, -1, 1, 1, 1, 1,
    1, 1, -1, 1, 1, 1, -1, 1, 1, -1, -1, 1, 1, 1, -1, 1, -1, -1, -1, 1, -1, 1, -1, -1, 1, -1, -1,
    1, 1, 1, 1, 1, -1, -1, 1, 1, -1, -1, 1, -1, 1, -1, 1, 1, -1, -1, -1, 1, 1, -1, -1, -1, -1, 1,
    -1, -1, 1, -1, 1, 1, 1, 1, -1, 1, -1, 1, -1, 1, -1, -1, -1, -1, -1, 1, -1, 1, 1, -1, 1, -1, 1,
    1, 1, -1, -1, 1, -1, -1, -1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];

/// 802.11a long training field, frequency domain, DC at index 32.
pub const LTF_A: [i8; 64] = [
    0, 0, 0, 0, 0, 0, 1, 1, -1, -1, 1, 1, -1, 1, -1, 1, 1, 1, 1, 1, 1, -1, -1, 1, 1, -1, 1, -1, 1,
    1, 1, 1, 0, 1, -1, -1, 1, 1, -1, 1, -1, 1, -1, -1, -1, -1, -1, 1, 1, -1, -1, 1, -1, 1, -1, 1,
    1, 1, 1, 0, 0, 0, 0, 0,
];

/// 802.11a long training field, time domain, conjugated: the taps of the
/// matched filter that finds it.
pub const LTF_A_MATCHED: [Complex32; 64] = [
    Complex32::new(1.3868, -0.0000),
    Complex32::new(-0.0455, 1.0679),
    Complex32::new(0.3528, 0.9865),
    Complex32::new(0.8594, -0.7348),
    Complex32::new(0.1874, -0.2475),
    Complex32::new(0.5309, 0.7784),
    Complex32::new(-1.0218, 0.4897),
    Complex32::new(-0.3401, 0.9423),
    Complex32::new(0.8657, 0.2298),
    Complex32::new(0.4734, -0.0362),
    Complex32::new(0.0088, 1.0207),
    Complex32::new(-1.2142, 0.4205),
    Complex32::new(0.2172, 0.5195),
    Complex32::new(0.5207, 0.1326),
    Complex32::new(-0.1995, -1.4259),
    Complex32::new(1.0583, 0.0363),
    Complex32::new(0.5547, 0.5547),
    Complex32::new(0.3277, -0.8728),
    Complex32::new(-0.5077, -0.3488),
    Complex32::new(-1.1650, -0.5789),
    Complex32::new(0.7297, -0.8197),
    Complex32::new(0.6173, -0.1253),
    Complex32::new(-0.5353, -0.7214),
    Complex32::new(-0.5011, 0.1935),
    Complex32::new(-0.3110, 1.3392),
    Complex32::new(-1.0818, 0.1470),
    Complex32::new(-1.1300, 0.1820),
    Complex32::new(0.6663, 0.6571),
    Complex32::new(-0.0249, -0.4773),
    Complex32::new(-0.8155, -1.0218),
    Complex32::new(0.8140, -0.9396),
    Complex32::new(0.1090, -0.8662),
    Complex32::new(-1.3868, -0.0000),
    Complex32::new(0.1090, 0.8662),
    Complex32::new(0.8140, 0.9396),
    Complex32::new(-0.8155, 1.0218),
    Complex32::new(-0.0249, 0.4773),
    Complex32::new(0.6663, -0.6571),
    Complex32::new(-1.1300, -0.1820),
    Complex32::new(-1.0818, -0.1470),
    Complex32::new(-0.3110, -1.3392),
    Complex32::new(-0.5011, -0.1935),
    Complex32::new(-0.5353, 0.7214),
    Complex32::new(0.6173, 0.1253),
    Complex32::new(0.7297, 0.8197),
    Complex32::new(-1.1650, 0.5789),
    Complex32::new(-0.5077, 0.3488),
    Complex32::new(0.3277, 0.8728),
    Complex32::new(0.5547, -0.5547),
    Complex32::new(1.0583, -0.0363),
    Complex32::new(-0.1995, 1.4259),
    Complex32::new(0.5207, -0.1326),
    Complex32::new(0.2172, -0.5195),
    Complex32::new(-1.2142, -0.4205),
    Complex32::new(0.0088, -1.0207),
    Complex32::new(0.4734, 0.0362),
    Complex32::new(0.8657, -0.2298),
    Complex32::new(-0.3401, -0.9423),
    Complex32::new(-1.0218, -0.4897),
    Complex32::new(0.5309, -0.7784),
    Complex32::new(0.1874, 0.2475),
    Complex32::new(0.8594, 0.7348),
    Complex32::new(0.3528, -0.9865),
    Complex32::new(-0.0455, -1.0679),
];

/// 802.11ah S1G long training field on subcarriers -28..=28 without DC.
pub const LTF_AH: [i8; 56] = [
    1, 1, 1, 1, -1, -1, 1, 1, -1, 1, -1, 1, 1, 1, 1, 1, 1, -1, -1, 1, 1, -1, 1, -1, 1, 1, 1, 1, 1,
    -1, -1, 1, 1, -1, 1, -1, 1, -1, -1, -1, -1, -1, 1, 1, -1, -1, 1, -1, 1, -1, 1, 1, 1, 1, -1, -1,
];
