#![allow(clippy::needless_range_loop)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::neg_multiply)]

use futuresdr::prelude::*;
use num_complex::Complex32;

// ============================================================
// Constants
// ============================================================

pub const MAX_PAYLOAD_SIZE: usize = 1500;
pub const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28; // MAC, CRC
pub const MAX_SYM: usize = ((16 + 8 * MAX_PSDU_SIZE + 6) / 24) + 1;
pub const MAX_ENCODED_BITS: usize = (16 + 8 * MAX_PSDU_SIZE + 6) * 2 + 288;

// ============================================================
// Modulation
// ============================================================

#[derive(Clone, Copy, Debug)]
pub enum Modulation {
    Bpsk,
    Qpsk,
    Qam16,
    Qam64,
}

impl Modulation {
    /// bits per symbol
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

// ============================================================
// Mcs
// ============================================================

#[derive(Clone, Copy, Debug)]
#[allow(non_camel_case_types)]
pub enum Mcs {
    Bpsk_1_2,
    Bpsk_3_4,
    Qpsk_1_2,
    Qpsk_3_4,
    Qam16_1_2,
    Qam16_3_4,
    Qam64_2_3,
    Qam64_3_4,
}

impl Mcs {
    pub fn depuncture_pattern(&self) -> &'static [usize] {
        match self {
            Mcs::Bpsk_1_2 | Mcs::Qpsk_1_2 | Mcs::Qam16_1_2 => &[1, 1],
            Mcs::Bpsk_3_4 | Mcs::Qpsk_3_4 | Mcs::Qam16_3_4 | Mcs::Qam64_3_4 => &[1, 1, 1, 0, 0, 1],
            Mcs::Qam64_2_3 => &[1, 1, 1, 0],
        }
    }

    pub fn modulation(&self) -> Modulation {
        match self {
            Mcs::Bpsk_1_2 => Modulation::Bpsk,
            Mcs::Bpsk_3_4 => Modulation::Bpsk,
            Mcs::Qpsk_1_2 => Modulation::Qpsk,
            Mcs::Qpsk_3_4 => Modulation::Qpsk,
            Mcs::Qam16_1_2 => Modulation::Qam16,
            Mcs::Qam16_3_4 => Modulation::Qam16,
            Mcs::Qam64_2_3 => Modulation::Qam64,
            Mcs::Qam64_3_4 => Modulation::Qam64,
        }
    }

    // coded bits per symbol
    pub fn n_cbps(&self) -> usize {
        match self {
            Mcs::Bpsk_1_2 => 48,
            Mcs::Bpsk_3_4 => 48,
            Mcs::Qpsk_1_2 => 96,
            Mcs::Qpsk_3_4 => 96,
            Mcs::Qam16_1_2 => 192,
            Mcs::Qam16_3_4 => 192,
            Mcs::Qam64_2_3 => 288,
            Mcs::Qam64_3_4 => 288,
        }
    }

    // data bits per symbol
    pub fn n_dbps(&self) -> usize {
        match self {
            Mcs::Bpsk_1_2 => 24,
            Mcs::Bpsk_3_4 => 36,
            Mcs::Qpsk_1_2 => 48,
            Mcs::Qpsk_3_4 => 72,
            Mcs::Qam16_1_2 => 96,
            Mcs::Qam16_3_4 => 144,
            Mcs::Qam64_2_3 => 192,
            Mcs::Qam64_3_4 => 216,
        }
    }

    // rate field for signal field
    pub fn rate_field(&self) -> u8 {
        match self {
            Mcs::Bpsk_1_2 => 0x0d,
            Mcs::Bpsk_3_4 => 0x0f,
            Mcs::Qpsk_1_2 => 0x05,
            Mcs::Qpsk_3_4 => 0x07,
            Mcs::Qam16_1_2 => 0x09,
            Mcs::Qam16_3_4 => 0x0b,
            Mcs::Qam64_2_3 => 0x01,
            Mcs::Qam64_3_4 => 0x03,
        }
    }

    pub fn index(&self) -> u8 {
        *self as u8
    }

    pub fn parse(s: &str) -> Result<Mcs, String> {
        let mut m = s.to_string().replace(['-', '_'], "");
        m.make_ascii_lowercase();
        match m.as_str() {
            "bpsk12" => Ok(Mcs::Bpsk_1_2),
            "bpsk34" => Ok(Mcs::Bpsk_3_4),
            "qpsk12" => Ok(Mcs::Qpsk_1_2),
            "qpsk34" => Ok(Mcs::Qpsk_3_4),
            "qam1612" => Ok(Mcs::Qam16_1_2),
            "qam1634" => Ok(Mcs::Qam16_3_4),
            "qam6423" => Ok(Mcs::Qam64_2_3),
            "qam6434" => Ok(Mcs::Qam64_3_4),
            _ => Err(format!("Invalid MCS {s}")),
        }
    }
}

// ============================================================
// FrameParam
// ============================================================

#[derive(Clone, Debug)]
pub struct FrameParam {
    mcs: Mcs,
    psdu_size: usize,
    n_data_bits: usize,
    n_symbols: usize,
    n_pad: usize,
}

impl FrameParam {
    pub fn new(mcs: Mcs, psdu_size: usize) -> Self {
        // n_symbols
        let bits = 16 + 8 * psdu_size + 6;
        let mut n_symbols = bits / mcs.n_dbps();
        if !bits.is_multiple_of(mcs.n_dbps()) {
            n_symbols += 1;
        }

        // n_pad
        let n_data_bits = n_symbols * mcs.n_dbps();
        let n_pad = n_data_bits - (16 + 8 * psdu_size + 6);

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

    /// Encode as 5 bytes [mcs_index, psdu_size as u32 LE] for cross-plugin tags.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = vec![self.mcs.index()];
        v.extend_from_slice(&(self.psdu_size as u32).to_le_bytes());
        v
    }
}

// ============================================================
// POLARITY constant (127 Complex32 values)
// ============================================================

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

// ============================================================
// LONG constant (64 Complex32 values)
// ============================================================

pub const LONG: [Complex32; 64] = [
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
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
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
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
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(-1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(1.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
    Complex32::new(0.0, 0.0),
];

// ============================================================
// ViterbiDecoder
// ============================================================

const TRACEBACK_MAX: usize = 24;

pub struct ViterbiDecoder {
    frame_param: FrameParam,
    n_traceback: usize,
    store_pos: usize,

    metric0: [u8; 64],
    metric1: [u8; 64],
    path0: [u8; 64],
    path1: [u8; 64],

    branchtab27: [[u8; 32]; 2],

    mmresult: [u8; 64],
    ppresult: [[u8; 64]; TRACEBACK_MAX],

    depunctured: [u8; MAX_ENCODED_BITS],
}

impl ViterbiDecoder {
    pub fn new() -> Self {
        ViterbiDecoder {
            frame_param: FrameParam::new(Mcs::Bpsk_1_2, 0),
            n_traceback: 0,
            store_pos: 0,

            metric0: [0; 64],
            metric1: [0; 64],
            path0: [0; 64],
            path1: [0; 64],

            branchtab27: [[0; 32]; 2],

            mmresult: [0; 64],
            ppresult: [[0; 64]; TRACEBACK_MAX],

            depunctured: [0; MAX_ENCODED_BITS],
        }
    }

    fn reset(&mut self, param: FrameParam) {
        self.frame_param = param;

        self.metric0.fill(0);
        self.metric1.fill(0);
        self.path0.fill(0);
        self.path1.fill(0);

        let polys: [usize; 2] = [0x6d, 0x4f];
        for i in 0..32 {
            self.branchtab27[0][i] = u8::from(PARTAB[(2 * i) & polys[0]] > 0);
            self.branchtab27[1][i] = u8::from(PARTAB[(2 * i) & polys[1]] > 0);
        }

        self.store_pos = 0;
        self.mmresult.fill(0);
        self.ppresult.fill([0; 64]);

        match self.frame_param.mcs() {
            Mcs::Bpsk_1_2 | Mcs::Qpsk_1_2 | Mcs::Qam16_1_2 => {
                self.n_traceback = 5;
            }
            Mcs::Bpsk_3_4 | Mcs::Qpsk_3_4 | Mcs::Qam16_3_4 | Mcs::Qam64_3_4 => {
                self.n_traceback = 10;
            }
            Mcs::Qam64_2_3 => {
                self.n_traceback = 9;
            }
        }
    }

    pub fn depuncture(&mut self, in_bits: &[u8]) {
        if self.n_traceback == 5 {
            self.depunctured[0..in_bits.len()].copy_from_slice(in_bits);
        } else {
            let pattern = self.frame_param.mcs.depuncture_pattern();
            let n_cbps = self.frame_param.mcs().n_cbps();
            let mut count = 0;

            for i in 0..self.frame_param.n_symbols() {
                for k in 0..n_cbps {
                    while pattern[count % pattern.len()] == 0 {
                        self.depunctured[count] = 2;
                        count += 1;
                    }

                    // Insert received bits
                    self.depunctured[count] = in_bits[i * n_cbps + k];
                    count += 1;

                    while pattern[count % pattern.len()] == 0 {
                        self.depunctured[count] = 2;
                        count += 1;
                    }
                }
            }
        }
    }

    fn viterbi_butterfly2_generic(&mut self, symbols: &[u8; 4]) {
        let mut metric0 = &mut self.metric0;
        let mut path0 = &mut self.path0;
        let mut metric1 = &mut self.metric1;
        let mut path1 = &mut self.path1;

        let mut m0 = [0u8; 16];
        let mut m1 = [0u8; 16];
        let mut m2 = [0u8; 16];
        let mut m3 = [0u8; 16];
        let mut decision0 = [0u8; 16];
        let mut decision1 = [0u8; 16];
        let mut survivor0 = [0u8; 16];
        let mut survivor1 = [0u8; 16];
        let mut metsv = [0u8; 16];
        let mut metsvm = [0u8; 16];
        let mut shift0 = [0u8; 16];
        let mut shift1 = [0u8; 16];
        let mut tmp0 = [0u8; 16];
        let mut tmp1 = [0u8; 16];
        let mut sym0v = [0u8; 16];
        let mut sym1v = [0u8; 16];
        let mut simd_epi16: u16;

        sym0v[0..16].fill(symbols[0]);
        sym1v[0..16].fill(symbols[1]);

        for i in 0..2 {
            if symbols[0] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[1][(i * 16) + j] ^ sym1v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else if symbols[1] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[0][(i * 16) + j] ^ sym0v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else {
                for j in 0..16 {
                    metsvm[j] = (self.branchtab27[0][(i * 16) + j] ^ sym0v[j])
                        + (self.branchtab27[1][(i * 16) + j] ^ sym1v[j]);
                    metsv[j] = 2 - metsvm[j];
                }
            }
            for j in 0..16 {
                m0[j] = metric0[(i * 16) + j] + metsv[j];
                m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
                m2[j] = metric0[(i * 16) + j] + metsvm[j];
                m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            }

            for j in 0..16 {
                decision0[j] = if m0[j] > m1[j] { 0xff } else { 0x0 };
                decision1[j] = if m2[j] > m3[j] { 0xff } else { 0x0 };
                survivor0[j] = (decision0[j] & m0[j]) | ((!decision0[j]) & m1[j]);
                survivor1[j] = (decision1[j] & m2[j]) | ((!decision1[j]) & m3[j]);
            }
            for j in (0..16).step_by(2) {
                simd_epi16 = path0[(i * 16) + j] as u16;
                simd_epi16 |= (path0[(i * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift0[j] = simd_epi16 as u8;
                shift0[j + 1] = (simd_epi16 >> 8) as u8;

                simd_epi16 = path0[((i + 2) * 16) + j] as u16;
                simd_epi16 |= (path0[((i + 2) * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift1[j] = simd_epi16 as u8;
                shift1[j + 1] = (simd_epi16 >> 8) as u8;
            }

            for j in 0..16 {
                shift1[j] += 1;
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                metric1[(2 * i * 16) + j] = survivor0[k];
                metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            }

            for j in 0..16 {
                tmp0[j] = (decision0[j] & shift0[j]) | ((!decision0[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                metric1[((2 * i + 1) * 16) + j] = survivor0[k];
                metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp1[j] = (decision1[j] & shift0[j]) | ((!decision1[j]) & shift1[j]);
            }

            for (j, k) in (0..16).step_by(2).zip(0..) {
                path1[(2 * i * 16) + j] = tmp0[k];
                path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                path1[((2 * i + 1) * 16) + j] = tmp0[k];
                path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            }
        }

        metric0 = &mut self.metric1;
        path0 = &mut self.path1;
        metric1 = &mut self.metric0;
        path1 = &mut self.path0;

        sym0v[0..16].fill(symbols[2]);
        sym1v[0..16].fill(symbols[3]);

        for i in 0..2 {
            if symbols[2] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[1][(i * 16) + j] ^ sym1v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else if symbols[3] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[0][(i * 16) + j] ^ sym0v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else {
                for j in 0..16 {
                    metsvm[j] = (self.branchtab27[0][(i * 16) + j] ^ sym0v[j])
                        + (self.branchtab27[1][(i * 16) + j] ^ sym1v[j]);
                    metsv[j] = 2 - metsvm[j];
                }
            }
            for j in 0..16 {
                m0[j] = metric0[(i * 16) + j] + metsv[j];
                m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
                m2[j] = metric0[(i * 16) + j] + metsvm[j];
                m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            }
            for j in 0..16 {
                decision0[j] = if m0[j] > m1[j] { 0xff } else { 0x0 };
                decision1[j] = if m2[j] > m3[j] { 0xff } else { 0x0 };
                survivor0[j] = (decision0[j] & m0[j]) | ((!decision0[j]) & m1[j]);
                survivor1[j] = (decision1[j] & m2[j]) | ((!decision1[j]) & m3[j]);
            }
            for j in (0..16).step_by(2) {
                simd_epi16 = path0[(i * 16) + j] as u16;
                simd_epi16 |= (path0[(i * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift0[j] = simd_epi16 as u8;
                shift0[j + 1] = (simd_epi16 >> 8) as u8;

                simd_epi16 = path0[((i + 2) * 16) + j] as u16;
                simd_epi16 |= (path0[((i + 2) * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift1[j] = simd_epi16 as u8;
                shift1[j + 1] = (simd_epi16 >> 8) as u8;
            }
            for j in 0..16 {
                shift1[j] += 1;
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                metric1[(2 * i * 16) + j] = survivor0[k];
                metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp0[j] = (decision0[j] & shift0[j]) | ((!decision0[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                metric1[((2 * i + 1) * 16) + j] = survivor0[k];
                metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp1[j] = (decision1[j] & shift0[j]) | ((!decision1[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                path1[(2 * i * 16) + j] = tmp0[k];
                path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                path1[((2 * i + 1) * 16) + j] = tmp0[k];
                path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            }
        }
    }

    fn viterbi_get_output_generic(&mut self) -> u8 {
        let mm0 = &mut self.metric0;
        let pp0 = &mut self.path0;

        self.store_pos = (self.store_pos + 1) % self.n_traceback;

        for i in 0..4 {
            for j in 0..16 {
                self.mmresult[(i * 16) + j] = mm0[(i * 16) + j];
                self.ppresult[self.store_pos][(i * 16) + j] = pp0[(i * 16) + j];
            }
        }

        // Find out the best final state
        let mut beststate = 0;
        let mut bestmetric = self.mmresult[beststate];
        let mut minmetric = self.mmresult[beststate];

        for i in 1..64 {
            if self.mmresult[i] > bestmetric {
                bestmetric = self.mmresult[i];
                beststate = i;
            }
            if self.mmresult[i] < minmetric {
                minmetric = self.mmresult[i];
            }
        }

        let mut pos = self.store_pos;
        for _ in 0..(self.n_traceback - 1) {
            // Obtain the state from the output bits
            // by clocking in the output bits in reverse order.
            // The state has only 6 bits
            beststate = (self.ppresult[pos][beststate] >> 2) as usize;
            pos = (pos + self.n_traceback - 1) % self.n_traceback;
        }

        for i in 0..4 {
            for j in 0..16 {
                pp0[(i * 16) + j] = 0;
                mm0[(i * 16) + j] -= minmetric;
            }
        }

        self.ppresult[pos][beststate]
    }

    pub fn decode(&mut self, frame: FrameParam, in_bits: &[u8], out_bits: &mut [u8]) {
        self.reset(frame);

        self.depuncture(in_bits);

        let mut in_count = 0;
        let mut out_count = 0;
        let mut n_decoded = 0;

        while n_decoded < self.frame_param.n_data_bits() {
            if (in_count % 4) == 0 {
                let index = in_count & !0b11;
                self.viterbi_butterfly2_generic(
                    &self.depunctured[index..index + 4].try_into().unwrap(),
                );

                if (in_count > 0) && (in_count % 16) == 8 {
                    let c = self.viterbi_get_output_generic();

                    if out_count >= self.n_traceback {
                        for i in 0..8 {
                            out_bits[(out_count - self.n_traceback) * 8 + i] = (c >> (7 - i)) & 0x1;
                            n_decoded += 1;
                        }
                    }
                    out_count += 1;
                }
            }
            in_count += 1;
        }
    }
}

impl Default for ViterbiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/* Parity lookup table */
const PARTAB: [u8; 256] = [
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
];

// ============================================================
// Equalizer (internal helper)
// ============================================================

const INTERLEAVER_PATTERN: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

struct Equalizer {
    h: [Complex32; 64],
    snr: f32,
}

impl Equalizer {
    fn new() -> Self {
        Equalizer {
            h: [Complex32::new(0.0, 0.0); 64],
            snr: 0.0,
        }
    }
    fn sync1(&mut self, s: &[Complex32; 64]) {
        self.h.copy_from_slice(s);
    }
    fn sync2(&mut self, s: &[Complex32; 64]) {
        let mut signal = 0.0f32;
        let mut noise = 0.0f32;
        for i in 6..=58 {
            if i == 32 {
                continue;
            }
            noise += (self.h[i] - s[i]).norm_sqr();
            signal += (self.h[i] + s[i]).norm_sqr();

            self.h[i] += s[i];
            self.h[i] /= LONG[i] + LONG[i];
        }
        self.snr = 10.0 * (signal / noise / 2.0).log10();
    }

    fn equalize(
        &mut self,
        input: &[Complex32; 64],
        output_symbols: &mut [Complex32; 48],
        output_bits: &mut [u8; 48],
        modulation: Modulation,
    ) {
        for (o, i) in (6..=58)
            .filter(|x| ![11, 25, 32, 39, 53].contains(x))
            .enumerate()
        {
            output_symbols[o] = input[i] / self.h[i];
            output_bits[o] = modulation.demap(&output_symbols[o]);
        }
    }

    fn snr(&self) -> f32 {
        self.snr
    }
}

// ============================================================
// FrameEqualizer Block
// ============================================================

#[derive(Debug)]
enum State {
    Sync1,
    Sync2,
    Signal,
    Copy(usize, usize, Modulation),
    Skip,
}

#[derive(Block)]
#[message_outputs(symbols)]
pub struct FrameEqualizer<I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<u8>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    #[input]
    input: I,
    #[output]
    output: O,
    equalizer: Equalizer,
    state: State,
    sym_in: [Complex32; 64],
    sym_out: [Complex32; 48],
    decoded_bits: [u8; 24],
    bits_out: [u8; 48],
    decoder: ViterbiDecoder,
    syms: Vec<Complex32>,
}

impl<I, O> FrameEqualizer<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            equalizer: Equalizer::new(),
            state: State::Skip,
            sym_in: [Complex32::new(0.0, 0.0); 64],
            sym_out: [Complex32::new(0.0, 0.0); 48],
            decoded_bits: [0; 24],
            bits_out: [0; 48],
            decoder: ViterbiDecoder::new(),
            syms: Vec::new(),
        }
    }

    fn decode_signal_field(
        decoder: &mut ViterbiDecoder,
        bits: &[u8; 48],
        decoded_bits: &mut [u8; 24],
    ) -> Option<FrameParam> {
        let mut deinterleaved = [0u8; 48];
        for i in 0..48 {
            deinterleaved[i] = bits[INTERLEAVER_PATTERN[i]];
        }

        decoder.decode(
            FrameParam::new(Mcs::Bpsk_1_2, 0),
            &deinterleaved,
            decoded_bits,
        );

        let mut r = 0;
        let mut bytes = 0;
        let mut parity = false;
        for i in 0..17 {
            parity ^= decoded_bits[i] > 0;

            if (i < 4) && (decoded_bits[i] > 0) {
                r |= 1 << i;
            }

            if (decoded_bits[i] > 0) && (i > 4) && (i < 17) {
                bytes |= 1 << (i - 5);
            }
        }

        if parity as u8 != decoded_bits[17] {
            return None;
        }

        match r {
            11 => Some(FrameParam::new(Mcs::Bpsk_1_2, bytes)),
            15 => Some(FrameParam::new(Mcs::Bpsk_3_4, bytes)),
            10 => Some(FrameParam::new(Mcs::Qpsk_1_2, bytes)),
            14 => Some(FrameParam::new(Mcs::Qpsk_3_4, bytes)),
            9 => Some(FrameParam::new(Mcs::Qam16_1_2, bytes)),
            13 => Some(FrameParam::new(Mcs::Qam16_3_4, bytes)),
            8 => Some(FrameParam::new(Mcs::Qam64_2_3, bytes)),
            12 => Some(FrameParam::new(Mcs::Qam64_3_4, bytes)),
            _ => {
                info!("signal: wrong encoding (r = {})", r);
                None
            }
        }
    }
}

impl<I, O> Default for FrameEqualizer<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I, O> Kernel for FrameEqualizer<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let (mut input, in_tags) = self.input.slice_with_tags();
        let (out, mut out_tags) = self.output.slice_with_tags();

        if let Some((index, _freq)) = in_tags.iter().find_map(|x| match x {
            ItemTag {
                index,
                tag: Tag::NamedF32(n, f),
            } => {
                if n == "wifi_start" {
                    Some((index, f))
                } else {
                    None
                }
            }
            _ => None,
        }) {
            if *index == 0 {
                if !matches!(self.state, State::Skip) {
                    info!("frame equalizer: canceling frame");
                }
                self.state = State::Sync1;
            } else {
                input = &input[0..*index];
            }
        }

        let max_i = input.len() / 64;
        let max_o = out.len() / 48;
        let mut i = 0;
        let mut o = 0;

        while i < max_i {
            // copy symbol w/ fft shift
            for k in 0..64 {
                let m = (k + 32) % 64;
                self.sym_in[m] = input[i * 64 + k];
            }

            match self.state {
                State::Sync1 | State::Sync2 => {
                    let beta =
                        (self.sym_in[11] - self.sym_in[25] + self.sym_in[39] + self.sym_in[53])
                            .arg();
                    for i in 0..64 {
                        self.sym_in[i] *= Complex32::from_polar(1.0, -beta);
                    }
                }
                State::Signal => {
                    let p = POLARITY[0];
                    let beta = ((self.sym_in[11] * p)
                        + (self.sym_in[39] * p)
                        + (self.sym_in[25] * p)
                        + (self.sym_in[53] * -p))
                        .arg();
                    for i in 0..64 {
                        self.sym_in[i] *= Complex32::from_polar(1.0, -beta);
                    }
                }
                State::Copy(left, n, _) => {
                    let p = POLARITY[(n - left + 1) % 127];
                    let beta = ((self.sym_in[11] * p)
                        + (self.sym_in[39] * p)
                        + (self.sym_in[25] * p)
                        + (self.sym_in[53] * -p))
                        .arg();
                    for i in 0..64 {
                        self.sym_in[i] *= Complex32::from_polar(1.0, -beta);
                    }
                }
                _ => {}
            }

            match &mut self.state {
                State::Sync1 => {
                    self.equalizer.sync1(&self.sym_in);
                    self.state = State::Sync2;
                    i += 1;
                }
                State::Sync2 => {
                    self.equalizer.sync2(&self.sym_in);
                    self.state = State::Signal;
                    i += 1;
                }
                State::Signal => {
                    self.equalizer.equalize(
                        &self.sym_in,
                        &mut self.sym_out,
                        &mut self.bits_out,
                        Modulation::Bpsk,
                    );
                    i += 1;
                    if let Some(frame) = Self::decode_signal_field(
                        &mut self.decoder,
                        &self.bits_out,
                        &mut self.decoded_bits,
                    ) {
                        self.state = State::Copy(
                            frame.n_symbols(),
                            frame.n_symbols(),
                            frame.mcs().modulation(),
                        );
                        out_tags.add_tag(
                            o * 48,
                            Tag::Data(Pmt::Blob(frame.to_bytes())),
                        );
                    } else {
                        info!(
                            "signal field could not be decoded, snr {}",
                            self.equalizer.snr()
                        );
                        self.state = State::Skip;
                    }
                }
                &mut State::Copy(mut n_sym, ref mut all_sym, ref mut modulation) => {
                    if o < max_o {
                        self.equalizer.equalize(
                            &self.sym_in,
                            &mut self.sym_out,
                            (&mut out[o * 48..(o + 1) * 48]).try_into().unwrap(),
                            *modulation,
                        );

                        self.syms.extend_from_slice(&self.sym_out);

                        i += 1;
                        o += 1;

                        n_sym -= 1;
                        if n_sym == 0 {
                            if !self.syms.is_empty() {
                                mio.post("symbols", Pmt::VecCF32(std::mem::take(&mut self.syms)))
                                    .await?;
                            }
                            self.state = State::Skip;
                        } else {
                            self.state = State::Copy(n_sym, *all_sym, *modulation);
                        }
                    } else {
                        break;
                    }
                }
                State::Skip => {
                    i += 1;
                }
            }
        }

        self.input.consume(i * 64);
        self.output.produce(o * 48);

        if self.input.finished() && i == max_i {
            io.finished = true;
        }

        Ok(())
    }
}

// ============================================================
// Plugin export
// ============================================================

plugin_api::export_plugin! {
    name: "FrameEqualizer",
    description: "WLAN frame equalizer - channel estimation, equalization, and signal field decoding",
    config: (),
    create: |_cfg, _id| {
        FrameEqualizer::<DefaultCpuReader<Complex32>, DefaultCpuWriter<u8>>::new()
    }
}
