#![allow(clippy::needless_range_loop)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::neg_multiply)]
use futuresdr::num_complex::Complex32;

mod channels;
pub use channels::channel_to_freq;
pub use channels::parse_channel;

mod decoder;
pub use decoder::Decoder;

mod encoder;
pub use encoder::Encoder;

mod frame_equalizer;
pub use frame_equalizer::FrameEqualizer;

mod mac;
pub use mac::Mac;

mod mapper;
pub use mapper::Mapper;

mod moving_average;
pub use moving_average::MovingAverage;

mod prefix;
pub use prefix::Prefix;

mod sync_long;
pub use sync_long::SyncLong;

mod sync_short;
pub use sync_short::SyncShort;

mod viterbi_decoder;
pub use viterbi_decoder::ViterbiDecoder;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

// ── 802.11ah 2 MHz OFDM parameters ─────────────────────────────────────
// Change OVERSAMPLING to match source sample rate:
//   OVERSAMPLING = fs / 2 MHz  (e.g. 2 for 4 MSps, 1 for 2 MSps)

pub const OVERSAMPLING: usize = 2;

pub const FFT_SIZE: usize = 64 * OVERSAMPLING;  // Tu
pub const CP_LEN: usize = 16 * OVERSAMPLING;    // Tcp
pub const SYMBOL_LEN: usize = FFT_SIZE + CP_LEN; // Ts
pub const DC_INDEX: usize = FFT_SIZE / 2;

/// Number of active subcarriers (data + pilot): fixed at 56 regardless of oversampling.
pub const N_ACTIVE_SC: usize = 56;
/// Number of data subcarriers per OFDM data symbol.
pub const N_DATA_SC: usize = 52;
/// Number of pilot subcarriers.
pub const N_PILOT_SC: usize = 4;
/// Number of SIG data subcarriers (narrower guard band: ±26 instead of ±28).
pub const N_SIG_DATA_SC: usize = 48;

pub const MAX_PAYLOAD_SIZE: usize = 1500;
pub const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28; // MAC, CRC
pub const MAX_SYM: usize = ((8 + 8 * MAX_PSDU_SIZE + 6) / 26) + 1; // worst case: MCS 0
pub const MAX_ENCODED_BITS: usize = (8 + 8 * MAX_PSDU_SIZE + 6) * 2 + 288;

// ── Subcarrier index helpers ────────────────────────────────────────────
// All subcarrier positions are defined relative to DC (center).
// Absolute FFT bin index = DC_INDEX + offset.

/// Convert a subcarrier offset (relative to DC) to an absolute FFT bin index.
pub const fn sc(offset: i32) -> usize {
    (DC_INDEX as i32 + offset) as usize
}

/// Pilot subcarrier offsets from DC.
pub const PILOT_OFFSETS: [i32; N_PILOT_SC] = [-21, -7, 7, 21];

/// Pilot ψ values: [1, 1, 1, -1] for pilot subcarriers [-21, -7, +7, +21].
pub const PILOT_PSI: [f32; 4] = [1.0, 1.0, 1.0, -1.0];

/// Traveling pilot positions (Table 23-22), NSTS=1, 2 MHz S1G PPDU.
/// 14 rows (cycling through data symbols), 4 columns (one per pilot).
/// Values are offsets from DC.
pub const TRAVELING_PILOT_OFFSETS: [[i32; 4]; 14] = [
    [-28, -12,   4,  20],
    [-24,  -8,   8,  24],
    [-20,  -4,  12,  28],
    [-16,  -2,  16,  26],
    [-26, -14,   2,  14],
    [-22, -10,   6,  18],
    [-18,  -6,  10,  22],
    [-27, -11,   5,  21],
    [-23,  -7,   9,  25],
    [-19,  -3,  13,  23],
    [-15,   1,  17,  27],
    [-25, -13,  -1,  11],
    [-21,  -9,   3,  15],
    [-17,  -5,   7,  19],
];

/// Returns the 4 pilot subcarrier FFT bin indices for a given data symbol.
pub fn pilot_sc_for_symbol(sym_idx: usize, traveling: bool) -> [usize; 4] {
    if traveling {
        let offsets = &TRAVELING_PILOT_OFFSETS[sym_idx % 14];
        [sc(offsets[0]), sc(offsets[1]), sc(offsets[2]), sc(offsets[3])]
    } else {
        [sc(PILOT_OFFSETS[0]), sc(PILOT_OFFSETS[1]), sc(PILOT_OFFSETS[2]), sc(PILOT_OFFSETS[3])]
    }
}

/// Returns the 52 data subcarrier FFT bin indices for a given data symbol.
pub fn data_sc_for_symbol(sym_idx: usize, traveling: bool) -> [usize; N_DATA_SC] {
    let pilots = pilot_sc_for_symbol(sym_idx, traveling);
    let mut out = [0usize; N_DATA_SC];
    let mut j = 0;
    for off in -28i32..=28 {
        if off == 0 { continue; }
        let idx = sc(off);
        if !pilots.contains(&idx) {
            out[j] = idx;
            j += 1;
        }
    }
    debug_assert_eq!(j, N_DATA_SC);
    out
}

/// Returns the 48 SIG data subcarrier FFT bin indices (guard ±26, minus pilots).
pub fn sig_data_sc() -> [usize; N_SIG_DATA_SC] {
    let pilots = [sc(PILOT_OFFSETS[0]), sc(PILOT_OFFSETS[1]), sc(PILOT_OFFSETS[2]), sc(PILOT_OFFSETS[3])];
    let mut out = [0usize; N_SIG_DATA_SC];
    let mut j = 0;
    for off in -26i32..=26 {
        if off == 0 { continue; }
        let idx = sc(off);
        if !pilots.contains(&idx) {
            out[j] = idx;
            j += 1;
        }
    }
    debug_assert_eq!(j, N_SIG_DATA_SC);
    out
}

// ── Modulation ──────────────────────────────────────────────────────────

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

// ── MCS (802.11ah Table 23-42, 2 MHz, Nss=1) ───────────────────────────

#[derive(Clone, Copy, Debug)]
#[allow(non_camel_case_types)]
pub enum Mcs {
    Bpsk_1_2,   // MCS 0
    Qpsk_1_2,   // MCS 1
    Qpsk_3_4,   // MCS 2
    Qam16_1_2,  // MCS 3
    Qam16_3_4,  // MCS 4
    Qam64_2_3,  // MCS 5
    Qam64_3_4,  // MCS 6
    Qam64_5_6,  // MCS 7
}

impl Mcs {
    /// Parse MCS index from the SIG field (0..=7).
    pub fn from_mcs_index(idx: u8) -> Option<Mcs> {
        match idx {
            0 => Some(Mcs::Bpsk_1_2),
            1 => Some(Mcs::Qpsk_1_2),
            2 => Some(Mcs::Qpsk_3_4),
            3 => Some(Mcs::Qam16_1_2),
            4 => Some(Mcs::Qam16_3_4),
            5 => Some(Mcs::Qam64_2_3),
            6 => Some(Mcs::Qam64_3_4),
            7 => Some(Mcs::Qam64_5_6),
            _ => None, // MCS 8-9 (256-QAM) not supported
        }
    }

    pub fn mcs_index(&self) -> u8 {
        match self {
            Mcs::Bpsk_1_2 => 0,
            Mcs::Qpsk_1_2 => 1,
            Mcs::Qpsk_3_4 => 2,
            Mcs::Qam16_1_2 => 3,
            Mcs::Qam16_3_4 => 4,
            Mcs::Qam64_2_3 => 5,
            Mcs::Qam64_3_4 => 6,
            Mcs::Qam64_5_6 => 7,
        }
    }

    pub fn depuncture_pattern(&self) -> &'static [usize] {
        match self {
            Mcs::Bpsk_1_2 | Mcs::Qpsk_1_2 | Mcs::Qam16_1_2 => &[1, 1],
            Mcs::Qpsk_3_4 | Mcs::Qam16_3_4 | Mcs::Qam64_3_4 => &[1, 1, 1, 0, 0, 1],
            Mcs::Qam64_2_3 => &[1, 1, 1, 0],
            Mcs::Qam64_5_6 => &[1, 1, 1, 0, 0, 1, 1, 0, 0, 1],
        }
    }

    pub fn modulation(&self) -> Modulation {
        match self {
            Mcs::Bpsk_1_2 => Modulation::Bpsk,
            Mcs::Qpsk_1_2 | Mcs::Qpsk_3_4 => Modulation::Qpsk,
            Mcs::Qam16_1_2 | Mcs::Qam16_3_4 => Modulation::Qam16,
            Mcs::Qam64_2_3 | Mcs::Qam64_3_4 | Mcs::Qam64_5_6 => Modulation::Qam64,
        }
    }

    /// Coded bits per OFDM symbol (Ncbps = Nbpscs × 52 data subcarriers).
    pub fn n_cbps(&self) -> usize {
        self.modulation().n_bpsc() * N_DATA_SC
    }

    /// Data bits per OFDM symbol (Ndbps).
    pub fn n_dbps(&self) -> usize {
        match self {
            Mcs::Bpsk_1_2 => 26,
            Mcs::Qpsk_1_2 => 52,
            Mcs::Qpsk_3_4 => 78,
            Mcs::Qam16_1_2 => 104,
            Mcs::Qam16_3_4 => 156,
            Mcs::Qam64_2_3 => 208,
            Mcs::Qam64_3_4 => 234,
            Mcs::Qam64_5_6 => 260,
        }
    }

    pub fn parse(s: &str) -> Result<Mcs, String> {
        let mut m = s.to_string().replace(['-', '_'], "");
        m.make_ascii_lowercase();
        match m.as_str() {
            "bpsk12" | "mcs0" | "0" => Ok(Mcs::Bpsk_1_2),
            "qpsk12" | "mcs1" | "1" => Ok(Mcs::Qpsk_1_2),
            "qpsk34" | "mcs2" | "2" => Ok(Mcs::Qpsk_3_4),
            "qam1612" | "mcs3" | "3" => Ok(Mcs::Qam16_1_2),
            "qam1634" | "mcs4" | "4" => Ok(Mcs::Qam16_3_4),
            "qam6423" | "mcs5" | "5" => Ok(Mcs::Qam64_2_3),
            "qam6434" | "mcs6" | "6" => Ok(Mcs::Qam64_3_4),
            "qam6456" | "mcs7" | "7" => Ok(Mcs::Qam64_5_6),
            _ => Err(format!("Invalid MCS {s}")),
        }
    }
}

// ── Frame parameters ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct FrameParam {
    mcs: Mcs,
    psdu_size: usize,
    n_data_bits: usize,
    n_symbols: usize,
    n_pad: usize,
    pub aggregation: bool,
    pub traveling_pilots: bool,
    pub short_gi: bool,
    pub is_long: bool,
}

impl FrameParam {
    /// Simple constructor (non-aggregated, no traveling pilots, no short GI).
    pub fn new(mcs: Mcs, psdu_size: usize) -> Self {
        Self::with_options(mcs, psdu_size, false, false, false, false)
    }

    /// Full constructor with all SIG field options.
    ///
    /// * `length` — PSDU length in bytes (non-aggregated) or N_sym (aggregated).
    pub fn with_options(
        mcs: Mcs,
        length: usize,
        aggregation: bool,
        traveling_pilots: bool,
        short_gi: bool,
        is_long: bool,
    ) -> Self {
        let n_symbols;
        let psdu_size;

        if aggregation {
            n_symbols = length;
            psdu_size = (n_symbols * mcs.n_dbps() - 8 - 6) / 8;
        } else {
            psdu_size = length;
            // N_sym = ceil((8*psdu_length + Nservice + Ntail) / Ndbps)
            // Nservice = 8 bits, Ntail = 6 bits (802.11ah)
            let bits = 8 + 8 * psdu_size + 6;
            n_symbols = (bits + mcs.n_dbps() - 1) / mcs.n_dbps();
        }

        let n_data_bits = n_symbols * mcs.n_dbps();
        let n_pad = n_data_bits - (8 + 8 * psdu_size + 6);

        FrameParam {
            mcs,
            psdu_size,
            n_data_bits,
            n_symbols,
            n_pad,
            aggregation,
            traveling_pilots,
            short_gi,
            is_long,
        }
    }

    pub fn psdu_size(&self) -> usize { self.psdu_size }
    pub fn mcs(&self) -> Mcs { self.mcs }
    pub fn n_data_bits(&self) -> usize { self.n_data_bits }
    pub fn n_pad(&self) -> usize { self.n_pad }
    pub fn n_symbols(&self) -> usize { self.n_symbols }
}

// ── CRC-4 for SIG field (802.11ah) ─────────────────────────────────────

pub fn crc4(bits: &[u8]) -> u8 {
    let mut r: u8 = 0xf;
    for &b in bits {
        if (b ^ (r >> 3)) & 1 != 0 {
            r = (r << 1) ^ 0x3;
        } else {
            r <<= 1;
        }
        r &= 0xf;
    }
    r ^ 0xf
}

/// CRC-8 for A-MPDU delimiter.
pub fn crc8(bits: &[u8]) -> u8 {
    let mut r: u8 = 0xff;
    for &b in bits {
        if (b ^ (r >> 7)) & 1 != 0 {
            r = (r << 1) ^ 0x07;
        } else {
            r <<= 1;
        }
    }
    r ^ 0xff
}

// ── Pilot polarity sequence (Section 17.3.5.10, 127 elements) ──────────

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

// ── LTF frequency-domain sequence ──────────────────────────────────────
// 56 active subcarriers at offsets -28..-1, +1..+28 relative to DC.
// All values real: +1.0 or -1.0.

/// Frequency-domain LTF values for the 56 active subcarriers (offsets -28 to +28, no DC).
pub const LTF_FREQ: [f32; N_ACTIVE_SC] = [
    // [1, 1] (offsets -28, -27)
     1.0,  1.0,
    // ltf_left (offsets -26..-1)
     1.0,  1.0, -1.0, -1.0,  1.0,  1.0, -1.0,  1.0, -1.0,  1.0,
     1.0,  1.0,  1.0,  1.0,  1.0, -1.0, -1.0,  1.0,  1.0, -1.0,
     1.0, -1.0,  1.0,  1.0,  1.0,  1.0,
    // ltf_right (offsets +1..+26)
     1.0, -1.0, -1.0,  1.0,  1.0, -1.0,  1.0, -1.0,  1.0, -1.0,
    -1.0, -1.0, -1.0, -1.0,  1.0,  1.0, -1.0, -1.0,  1.0, -1.0,
     1.0, -1.0,  1.0,  1.0,  1.0,  1.0,
    // [-1, -1] (offsets +27, +28)
    -1.0, -1.0,
];

/// Build the FFT_SIZE-element frequency-domain LTF array (fftshift convention).
/// The returned array has LTF_FREQ values placed at the correct FFT bin indices.
pub fn long_freq_domain() -> Vec<Complex32> {
    let mut arr = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
    let mut k = 0;
    for off in -28i32..=28 {
        if off == 0 { continue; }
        arr[sc(off)] = Complex32::new(LTF_FREQ[k], 0.0);
        k += 1;
    }
    arr
}

/// Compute the time-domain LTF (FFT_SIZE samples) by IFFT of the frequency-domain LTF.
/// Used by SyncLong for correlation.
pub fn long_time_domain() -> Vec<Complex32> {
    let freq = long_freq_domain();

    // ifftshift: move DC from center to index 0
    let mut f_fft = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
    for i in 0..FFT_SIZE {
        f_fft[i] = freq[(i + DC_INDEX) % FFT_SIZE];
    }

    // IFFT: x[n] = (1/N) * Σ X[k] * exp(+j*2π*k*n/N)
    let n = FFT_SIZE as f32;
    let mut time = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
    for t in 0..FFT_SIZE {
        let mut sum = Complex32::new(0.0, 0.0);
        for k in 0..FFT_SIZE {
            let angle = 2.0 * std::f32::consts::PI * (k as f32) * (t as f32) / n;
            sum += f_fft[k] * Complex32::from_polar(1.0, angle);
        }
        time[t] = sum / n;
    }
    time
}
