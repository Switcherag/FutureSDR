//! Modulation, coding and frame parameters shared by 802.11a and 802.11ah.

use futuresdr::num_complex::Complex32;

use crate::Standard;

/// Largest MSDU, and the PSDU around it (MAC header and FCS).
pub const MAX_PAYLOAD_SIZE: usize = 1500;
pub const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28;

/// Tail bits of the convolutional code.
pub const TAIL_BITS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modulation {
    Bpsk,
    Qpsk,
    Qam16,
    Qam64,
}

impl Modulation {
    /// Coded bits per subcarrier.
    pub fn n_bpsc(self) -> usize {
        match self {
            Modulation::Bpsk => 1,
            Modulation::Qpsk => 2,
            Modulation::Qam16 => 4,
            Modulation::Qam64 => 6,
        }
    }

    /// Hard decision; bit `k` of the result is the `k`-th coded bit.
    pub fn demap(self, i: &Complex32) -> u8 {
        match self {
            Modulation::Bpsk => (i.re > 0.0) as u8,
            Modulation::Qpsk => 2 * (i.im > 0.0) as u8 + (i.re > 0.0) as u8,
            Modulation::Qam16 => {
                const LEVEL: f32 = 0.632_455_5;
                u8::from(i.re > 0.0)
                    | if i.re.abs() < LEVEL { 2 } else { 0 }
                    | if i.im > 0.0 { 4 } else { 0 }
                    | if i.im.abs() < LEVEL { 8 } else { 0 }
            }
            Modulation::Qam64 => {
                const LEVEL: f32 = 0.154_303_35;
                let bits = |v: f32| {
                    u8::from(v > 0.0)
                        | if v.abs() < 4.0 * LEVEL { 2 } else { 0 }
                        | if v.abs() < 6.0 * LEVEL && v.abs() > 2.0 * LEVEL {
                            4
                        } else {
                            0
                        }
                };
                bits(i.re) | bits(i.im) << 3
            }
        }
    }
}

/// Rate of the punctured convolutional code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeRate {
    R1_2,
    R2_3,
    R3_4,
    R5_6,
}

impl CodeRate {
    /// Coded bits kept (1) and punctured (0), repeating.
    pub fn puncturing(self) -> &'static [u8] {
        match self {
            CodeRate::R1_2 => &[1, 1],
            CodeRate::R2_3 => &[1, 1, 1, 0],
            CodeRate::R3_4 => &[1, 1, 1, 0, 0, 1],
            CodeRate::R5_6 => &[1, 1, 1, 0, 0, 1, 1, 0, 0, 1],
        }
    }

    /// Viterbi traceback depth, in bytes.
    pub fn traceback(self) -> usize {
        match self {
            CodeRate::R1_2 => 5,
            CodeRate::R2_3 => 9,
            CodeRate::R3_4 => 10,
            CodeRate::R5_6 => 12,
        }
    }

    fn times(self, n: usize) -> usize {
        match self {
            CodeRate::R1_2 => n / 2,
            CodeRate::R2_3 => n * 2 / 3,
            CodeRate::R3_4 => n * 3 / 4,
            CodeRate::R5_6 => n * 5 / 6,
        }
    }
}

/// Modulation and coding of a frame's data symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mcs {
    pub modulation: Modulation,
    pub rate: CodeRate,
}

impl Mcs {
    pub const fn new(modulation: Modulation, rate: CodeRate) -> Self {
        Self { modulation, rate }
    }

    /// Coded bits per OFDM symbol with `n_data_sc` data subcarriers.
    pub fn n_cbps(self, n_data_sc: usize) -> usize {
        self.modulation.n_bpsc() * n_data_sc
    }

    /// Data bits per OFDM symbol with `n_data_sc` data subcarriers.
    pub fn n_dbps(self, n_data_sc: usize) -> usize {
        self.rate.times(self.n_cbps(n_data_sc))
    }
}

/// What the signal field says about a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameParam {
    pub mcs: Mcs,
    pub psdu_size: usize,
    pub n_symbols: usize,
    pub n_data_bits: usize,
    /// Data subcarriers per symbol.
    pub n_data_sc: usize,
    /// Bits of the SERVICE field in front of the PSDU.
    pub service_bits: usize,
    /// 802.11ah: the PSDU is an A-MPDU.
    pub aggregation: bool,
    /// 802.11ah: pilots move from symbol to symbol.
    pub traveling_pilots: bool,
    /// 802.11ah: short guard interval.
    pub short_gi: bool,
    /// Symbols between the signal field and the data (802.11ah S1G long
    /// preamble).
    pub skip_symbols: usize,
}

impl FrameParam {
    /// A frame of `psdu_size` bytes.
    pub fn new<S: Standard>(mcs: Mcs, psdu_size: usize) -> Self {
        let n_dbps = mcs.n_dbps(S::N_DATA_SC);
        let bits = S::SERVICE_BITS + 8 * psdu_size + TAIL_BITS;
        Self::with_symbols::<S>(mcs, bits.div_ceil(n_dbps), psdu_size)
    }

    /// An A-MPDU of `n_symbols` symbols.
    pub fn aggregate<S: Standard>(mcs: Mcs, n_symbols: usize) -> Self {
        let n_dbps = mcs.n_dbps(S::N_DATA_SC);
        let psdu_size = (n_symbols * n_dbps).saturating_sub(S::SERVICE_BITS + TAIL_BITS) / 8;
        Self {
            aggregation: true,
            ..Self::with_symbols::<S>(mcs, n_symbols, psdu_size)
        }
    }

    fn with_symbols<S: Standard>(mcs: Mcs, n_symbols: usize, psdu_size: usize) -> Self {
        Self {
            mcs,
            psdu_size,
            n_symbols,
            n_data_bits: n_symbols * mcs.n_dbps(S::N_DATA_SC),
            n_data_sc: S::N_DATA_SC,
            service_bits: S::SERVICE_BITS,
            aggregation: false,
            traveling_pilots: false,
            short_gi: false,
            skip_symbols: 0,
        }
    }

    pub fn n_cbps(&self) -> usize {
        self.mcs.n_cbps(self.n_data_sc)
    }

    /// Whether a receiver can hold the frame.
    pub fn fits<S: Standard>(&self) -> bool {
        self.psdu_size <= MAX_PSDU_SIZE && (1..=S::MAX_SYMBOLS).contains(&self.n_symbols)
    }
}

/// `e^(j f n)` for `n` = 0, 1, ...: a frequency correction. Multiplies by
/// `e^(j f)` from one sample to the next, and takes the exact value every
/// 1024 samples.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rotation {
    f: f32,
    n: usize,
    rot: Complex32,
    step: Complex32,
}

impl Rotation {
    pub(crate) fn new(f: f32) -> Self {
        Self::at(f, 0)
    }

    /// Starting at `n`.
    pub(crate) fn at(f: f32, n: usize) -> Self {
        Self {
            f,
            n,
            rot: Complex32::from_polar(1.0, f * n as f32),
            step: Complex32::from_polar(1.0, f),
        }
    }

    #[inline]
    pub(crate) fn next(&mut self) -> Complex32 {
        let rot = self.rot;
        self.n += 1;
        self.rot = if self.n.is_multiple_of(1024) {
            Complex32::from_polar(1.0, self.f * self.n as f32)
        } else {
            rot * self.step
        };
        rot
    }
}

const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// CRC-32 of IEEE 802.3, as in the FCS.
pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, b| {
        CRC32_TABLE[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8)
    })
}

/// Whether `mpdu` ends with a correct FCS.
pub fn fcs_ok(mpdu: &[u8]) -> bool {
    // The CRC over data and FCS together is this constant.
    mpdu.len() > 4 && crc32(mpdu) == 0x2144_df1c
}

/// CRC-4 of the 802.11ah SIG field, over bits.
pub fn crc4(bits: &[u8]) -> u8 {
    let mut r: u8 = 0xf;
    for &b in bits {
        r = if (b ^ (r >> 3)) & 1 != 0 {
            (r << 1) ^ 0x3
        } else {
            r << 1
        } & 0xf;
    }
    r ^ 0xf
}

/// CRC-8 of an A-MPDU delimiter, over bits.
pub fn crc8(bits: &[u8]) -> u8 {
    let mut r: u8 = 0xff;
    for &b in bits {
        r = if (b ^ (r >> 7)) & 1 != 0 {
            (r << 1) ^ 0x07
        } else {
            r << 1
        };
    }
    r ^ 0xff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_is_the_ethernet_crc() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let mut frame = b"FutureSDR".to_vec();
        frame.extend(crc32(&frame).to_le_bytes());
        assert!(fcs_ok(&frame));
        frame[0] ^= 1;
        assert!(!fcs_ok(&frame));
    }

    #[test]
    fn data_bits_per_symbol() {
        use CodeRate::*;
        use Modulation::*;
        let a: Vec<usize> = [
            (Bpsk, R1_2),
            (Bpsk, R3_4),
            (Qpsk, R1_2),
            (Qpsk, R3_4),
            (Qam16, R1_2),
            (Qam16, R3_4),
            (Qam64, R2_3),
            (Qam64, R3_4),
        ]
        .into_iter()
        .map(|(m, r)| Mcs::new(m, r).n_dbps(48))
        .collect();
        assert_eq!(a, [24, 36, 48, 72, 96, 144, 192, 216]);
        assert_eq!(Mcs::new(Qam64, R5_6).n_dbps(52), 260);
        assert_eq!(Mcs::new(Bpsk, R1_2).n_dbps(52), 26);
    }

    #[test]
    fn qam64_demap_is_gray_coded_per_axis() {
        const LEVEL: f32 = 0.154_303_35;
        let expect = [0b000u8, 0b100, 0b110, 0b010, 0b011, 0b111, 0b101, 0b001];
        for (k, level) in [-7.0, -5.0, -3.0, -1.0, 1.0, 3.0, 5.0, 7.0]
            .iter()
            .enumerate()
        {
            let v = Complex32::new(level * LEVEL, -7.0 * LEVEL);
            assert_eq!(Modulation::Qam64.demap(&v) & 0b111, expect[k], "{level}");
        }
    }
}
