#![allow(clippy::needless_range_loop)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::neg_multiply)]
use futuresdr::num_complex::Complex32;

mod channels;
pub use channels::channel_to_freq;
pub use channels::parse_channel;

mod decoder;
pub use decoder::Decoder;

mod frame_equalizer;
pub use frame_equalizer::FrameEqualizer;

mod moving_average;
pub use moving_average::MovingAverage;

mod sync_long;
pub use sync_long::SyncLong;

mod sync_short;
pub use sync_short::SyncShort;

mod viterbi_decoder;
pub use viterbi_decoder::ViterbiDecoder;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

// 802.11ah 1 MHz OFDM parameters (IEEE Std 802.11ah-2016, Table 23-4)
pub const FFT_SIZE: usize = 32;
pub const N_DATA_SC: usize = 24; // NSD for 1 MHz
pub const N_PILOT_SC: usize = 2; // NSP for 1 MHz
pub const N_TOTAL_SC: usize = 26; // NST = NSD + NSP
pub const GI_SAMPLES: usize = 8; // TGI = 8 µs = 8 samples at 1 MHz
pub const SYMBOL_SAMPLES: usize = 40; // TSYML = 40 µs = 40 samples at 1 MHz
pub const N_SIG_SYMBOLS: usize = 6; // SIG field = 6 OFDM symbols for 1 MHz
pub const N_SERVICE_BITS: usize = 8; // Nservice = 8 for 802.11ah

pub const MAX_PAYLOAD_SIZE: usize = 1500;
pub const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28; // MAC, CRC
pub const MAX_SYM: usize = ((N_SERVICE_BITS + 8 * MAX_PSDU_SIZE + 6) / 12) + 1;
pub const MAX_ENCODED_BITS: usize = (N_SERVICE_BITS + 8 * MAX_PSDU_SIZE + 6) * 2 + 288;

#[derive(Clone, Copy, Debug)]
pub enum Modulation {
    Bpsk,
    Qpsk,
    Qam16,
    Qam64,
}

impl Modulation {
    /// bits per subcarrier per spatial stream (N_BPSCS)
    pub fn n_bpsc(&self) -> usize {
        match self {
            Modulation::Bpsk => 1,
            Modulation::Qpsk => 2,
            Modulation::Qam16 => 4,
            Modulation::Qam64 => 6,
        }
    }
    pub fn map(&self, i: u8) -> Complex32 {
        match self {
            Modulation::Bpsk => {
                const BPSK: [Complex32; 2] = [Complex32::new(-1.0, 0.0), Complex32::new(1.0, 0.0)];
                BPSK[i as usize]
            }
            Modulation::Qpsk => {
                const LEVEL: f32 = std::f32::consts::FRAC_1_SQRT_2;
                const QPSK: [Complex32; 4] = [
                    Complex32::new(-LEVEL, -LEVEL),
                    Complex32::new(LEVEL, -LEVEL),
                    Complex32::new(-LEVEL, LEVEL),
                    Complex32::new(LEVEL, LEVEL),
                ];
                QPSK[i as usize]
            }
            Modulation::Qam16 => {
                const LEVEL: f32 = 0.31622776601683794;
                const QAM16: [Complex32; 16] = [
                    Complex32::new(-3.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, 1.0 * LEVEL),
                ];
                QAM16[i as usize]
            }
            Modulation::Qam64 => {
                const LEVEL: f32 = 0.1543033499620919;
                const QAM64: [Complex32; 64] = [
                    Complex32::new(-7.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, -7.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, 7.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, -1.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, 1.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, -5.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, 5.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, -3.0 * LEVEL),
                    Complex32::new(-7.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(7.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(-1.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(1.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(-5.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(5.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(-3.0 * LEVEL, 3.0 * LEVEL),
                    Complex32::new(3.0 * LEVEL, 3.0 * LEVEL),
                ];
                QAM64[i as usize]
            }
        }
    }

    pub fn demap(&self, i: &Complex32) -> u8 {
        match self {
            Modulation::Bpsk => (i.re > 0.0) as u8,
            Modulation::Qpsk => 2 * (i.im > 0.0) as u8 + (i.re > 0.0) as u8,
            Modulation::Qam16 => {
                let mut ret = 0u8;
                const LEVEL: f32 = 0.6324555320336759;
                let re = i.re;
                let im = i.im;

                ret |= u8::from(re > 0.0);
                ret |= if re.abs() < LEVEL { 2 } else { 0 };
                ret |= if im > 0.0 { 4 } else { 0 };
                ret |= if im.abs() < LEVEL { 8 } else { 0 };
                ret
            }
            Modulation::Qam64 => {
                const LEVEL: f32 = 0.1543033499620919;

                let mut ret = 0;
                let re = i.re;
                let im = i.im;

                ret |= u8::from(re > 0.0);
                ret |= if re.abs() < (4.0 * LEVEL) { 2 } else { 0 };
                ret |= if (re.abs() < (6.0 * LEVEL)) && (re.abs() > (2.0 * LEVEL)) {
                    4
                } else {
                    0
                };
                ret |= if im > 0.0 { 8 } else { 0 };
                ret |= if im.abs() < (4.0 * LEVEL) { 16 } else { 0 };
                ret |= if (im.abs() < (6.0 * LEVEL)) && (im.abs() > (2.0 * LEVEL)) {
                    32
                } else {
                    0
                };

                ret
            }
        }
    }
}

/// 802.11ah MCS index (Table 23-41 for 1 MHz, 1 spatial stream)
#[derive(Clone, Copy, Debug)]
#[allow(non_camel_case_types)]
pub enum Mcs {
    Mcs0, // BPSK 1/2
    Mcs1, // QPSK 1/2
    Mcs2, // QPSK 3/4
    Mcs3, // 16-QAM 1/2
    Mcs4, // 16-QAM 3/4
    Mcs5, // 64-QAM 2/3
    Mcs6, // 64-QAM 3/4
    Mcs7, // 64-QAM 5/6
}

impl Mcs {
    pub fn depuncture_pattern(&self) -> &'static [usize] {
        match self {
            Mcs::Mcs0 | Mcs::Mcs1 | Mcs::Mcs3 => &[1, 1],                   // rate 1/2
            Mcs::Mcs2 | Mcs::Mcs4 | Mcs::Mcs6 => &[1, 1, 1, 0, 0, 1],      // rate 3/4
            Mcs::Mcs5 => &[1, 1, 1, 0],                                      // rate 2/3
            Mcs::Mcs7 => &[1, 1, 1, 0, 1, 0, 1, 0, 0, 1],                   // rate 5/6
        }
    }

    pub fn modulation(&self) -> Modulation {
        match self {
            Mcs::Mcs0 => Modulation::Bpsk,
            Mcs::Mcs1 | Mcs::Mcs2 => Modulation::Qpsk,
            Mcs::Mcs3 | Mcs::Mcs4 => Modulation::Qam16,
            Mcs::Mcs5 | Mcs::Mcs6 | Mcs::Mcs7 => Modulation::Qam64,
        }
    }

    /// Coded bits per OFDM symbol (N_CBPS) for 1 MHz
    pub fn n_cbps(&self) -> usize {
        N_DATA_SC * self.modulation().n_bpsc()
    }

    /// Data bits per OFDM symbol (N_DBPS) for 1 MHz
    pub fn n_dbps(&self) -> usize {
        match self {
            Mcs::Mcs0 => 12,  // BPSK 1/2:   24 * 1/2
            Mcs::Mcs1 => 24,  // QPSK 1/2:   48 * 1/2
            Mcs::Mcs2 => 36,  // QPSK 3/4:   48 * 3/4
            Mcs::Mcs3 => 48,  // 16QAM 1/2:  96 * 1/2
            Mcs::Mcs4 => 72,  // 16QAM 3/4:  96 * 3/4
            Mcs::Mcs5 => 96,  // 64QAM 2/3: 144 * 2/3
            Mcs::Mcs6 => 108, // 64QAM 3/4: 144 * 3/4
            Mcs::Mcs7 => 120, // 64QAM 5/6: 144 * 5/6
        }
    }

    /// MCS index for SIG field encoding
    pub fn index(&self) -> u8 {
        match self {
            Mcs::Mcs0 => 0,
            Mcs::Mcs1 => 1,
            Mcs::Mcs2 => 2,
            Mcs::Mcs3 => 3,
            Mcs::Mcs4 => 4,
            Mcs::Mcs5 => 5,
            Mcs::Mcs6 => 6,
            Mcs::Mcs7 => 7,
        }
    }

    pub fn from_index(idx: u8) -> Option<Mcs> {
        match idx {
            0 => Some(Mcs::Mcs0),
            1 => Some(Mcs::Mcs1),
            2 => Some(Mcs::Mcs2),
            3 => Some(Mcs::Mcs3),
            4 => Some(Mcs::Mcs4),
            5 => Some(Mcs::Mcs5),
            6 => Some(Mcs::Mcs6),
            7 => Some(Mcs::Mcs7),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Result<Mcs, String> {
        let mut m = s.to_string().replace(['-', '_'], "");
        m.make_ascii_lowercase();
        match m.as_str() {
            "mcs0" | "bpsk12" => Ok(Mcs::Mcs0),
            "mcs1" | "qpsk12" => Ok(Mcs::Mcs1),
            "mcs2" | "qpsk34" => Ok(Mcs::Mcs2),
            "mcs3" | "qam1612" => Ok(Mcs::Mcs3),
            "mcs4" | "qam1634" => Ok(Mcs::Mcs4),
            "mcs5" | "qam6423" => Ok(Mcs::Mcs5),
            "mcs6" | "qam6434" => Ok(Mcs::Mcs6),
            "mcs7" | "qam6456" => Ok(Mcs::Mcs7),
            _ => Err(format!("Invalid MCS {s}")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FrameParam {
    pub mcs: Mcs,
    psdu_size: usize,
    n_data_bits: usize,
    n_symbols: usize,
    n_pad: usize,
}

impl FrameParam {
    pub fn new(mcs: Mcs, psdu_size: usize) -> Self {
        let bits = N_SERVICE_BITS + 8 * psdu_size + 6;
        let mut n_symbols = bits / mcs.n_dbps();
        if bits % mcs.n_dbps() != 0 {
            n_symbols += 1;
        }

        let n_data_bits = n_symbols * mcs.n_dbps();
        let n_pad = n_data_bits - bits;

        FrameParam {
            mcs,
            psdu_size,
            n_data_bits,
            n_symbols,
            n_pad,
        }
    }
    pub fn psdu_size(&self) -> usize {
        self.psdu_size
    }

    pub fn mcs(&self) -> Mcs {
        self.mcs
    }

    pub fn n_data_bits(&self) -> usize {
        self.n_data_bits
    }

    pub fn n_pad(&self) -> usize {
        self.n_pad
    }

    pub fn n_symbols(&self) -> usize {
        self.n_symbols
    }
}

/// Pilot polarity sequence (same as 802.11a, from 17.3.5.10)
pub const POLARITY: [Complex32; 127] = [
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
];

/// 1 MHz S1G LTF frequency-domain values (IEEE 802.11ah-2016, Table 23-7)
/// 32-point: subcarriers -13 to +13, with DC=0, guards at ±14..±16
/// Layout (after FFT shift, centered at index 0):
///   [0]=DC, [1..13]=subcarriers +1..+13, [14..18]=guard, [19..31]=subcarriers -13..-1
pub const LONG: [Complex32; 32] = [
    Complex32::new(0.0, 0.0),  // 0: DC
    Complex32::new(1.0, 0.0),  // 1: subcarrier +1
    Complex32::new(-1.0, 0.0), // 2: subcarrier +2
    Complex32::new(-1.0, 0.0), // 3: subcarrier +3
    Complex32::new(1.0, 0.0),  // 4: subcarrier +4
    Complex32::new(1.0, 0.0),  // 5: subcarrier +5
    Complex32::new(-1.0, 0.0), // 6: subcarrier +6
    Complex32::new(1.0, 0.0),  // 7: subcarrier +7 (pilot position)
    Complex32::new(-1.0, 0.0), // 8: subcarrier +8
    Complex32::new(1.0, 0.0),  // 9: subcarrier +9
    Complex32::new(-1.0, 0.0), // 10: subcarrier +10
    Complex32::new(-1.0, 0.0), // 11: subcarrier +11
    Complex32::new(-1.0, 0.0), // 12: subcarrier +12
    Complex32::new(-1.0, 0.0), // 13: subcarrier +13
    Complex32::new(0.0, 0.0),  // 14: guard
    Complex32::new(0.0, 0.0),  // 15: guard
    Complex32::new(0.0, 0.0),  // 16: guard (= subcarrier -16)
    Complex32::new(0.0, 0.0),  // 17: guard (= subcarrier -15)
    Complex32::new(0.0, 0.0),  // 18: guard (= subcarrier -14)
    Complex32::new(1.0, 0.0),  // 19: subcarrier -13
    Complex32::new(1.0, 0.0),  // 20: subcarrier -12
    Complex32::new(-1.0, 0.0), // 21: subcarrier -11
    Complex32::new(-1.0, 0.0), // 22: subcarrier -10
    Complex32::new(1.0, 0.0),  // 23: subcarrier -9
    Complex32::new(1.0, 0.0),  // 24: subcarrier -8
    Complex32::new(-1.0, 0.0), // 25: subcarrier -7 (pilot position)
    Complex32::new(1.0, 0.0),  // 26: subcarrier -6
    Complex32::new(-1.0, 0.0), // 27: subcarrier -5
    Complex32::new(1.0, 0.0),  // 28: subcarrier -4
    Complex32::new(1.0, 0.0),  // 29: subcarrier -3
    Complex32::new(1.0, 0.0),  // 30: subcarrier -2
    Complex32::new(1.0, 0.0),  // 31: subcarrier -1
];

/// 1 MHz SIG field pilot polarity (IEEE 802.11ah-2016, p.3253)
/// For symbol n: pilot at -7 uses gamma[(n%2)+2], pilot at +7 uses gamma[((n+1)%2)+2]
/// gamma = [1, 1, 1, -1, -1, 1, 1, 1]
/// This gives alternating pattern for 6 SIG symbols
pub const SIG_PILOT_POLARITY: [(f32, f32); 6] = [
    (1.0, -1.0),   // n=0: -7 pilot = gamma[2]=1,  +7 pilot = gamma[3]=-1
    (-1.0, 1.0),   // n=1: -7 pilot = gamma[3]=-1, +7 pilot = gamma[2]=1
    (1.0, -1.0),   // n=2
    (-1.0, 1.0),   // n=3
    (1.0, -1.0),   // n=4
    (-1.0, 1.0),   // n=5
];
