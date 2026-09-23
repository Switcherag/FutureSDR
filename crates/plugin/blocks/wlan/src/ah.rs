//! IEEE 802.11ah, S1G 2 MHz at 4 MSps: the `dyn` branch's receiver v6.

use futuresdr::num_complex::Complex32;

use crate::CodeRate;
use crate::Deconvolve;
use crate::FrameParam;
use crate::Mcs;
use crate::Modulation;
use crate::Signal;
use crate::Standard;
use crate::SymbolEqualizer;
use crate::ViterbiDecoder;
use crate::crc4;
use crate::crc8;
use crate::fcs_ok;
use crate::tables::LTF_AH;
use crate::tables::POLARITY;

/// IEEE 802.11ah, S1G 2 MHz.
pub struct Ah;

impl Standard for Ah {
    const NAME: &'static str = "802.11ah";
    const FFT_SIZE: usize = 128;
    const CP_LEN: usize = 32;
    // Until the moving averages are full, their ratio is meaningless.
    const STF_WARMUP: usize = Self::STF_POWER_WIN + Self::STF_CORR_WIN;
    const MAX_SAMPLES: usize = (6 + Self::MAX_SYMBOLS) * Self::SYMBOL_LEN;
    const LTF_SEARCH: usize = 640;
    const N_DATA_SC: usize = 52;
    const INTERLEAVER_COLUMNS: usize = 13;
    const SERVICE_BITS: usize = 8;

    type Equalizer = Equalizer;

    /// The conjugated long training symbol, from its subcarriers by the
    /// inverse DFT.
    fn ltf_taps() -> Vec<Complex32> {
        let f = ltf_freq();
        let n = FFT as f32;
        (0..FFT)
            .map(|t| {
                let mut acc = Complex32::default();
                for k in 0..FFT {
                    let angle = 2.0 * std::f32::consts::PI * (k as f32) * (t as f32) / n;
                    acc += f[(k + DC) % FFT] * Complex32::from_polar(1.0, angle);
                }
                (acc / n).conj()
            })
            .collect()
    }

    fn mpdus(frame: &FrameParam, psdu: &[u8], out: &mut Vec<(Vec<u8>, bool)>) {
        if frame.aggregation {
            for mpdu in ampdu(psdu) {
                if fcs_ok(mpdu) {
                    out.push((mpdu[..mpdu.len() - 4].to_vec(), true));
                } else {
                    out.push((mpdu.to_vec(), false));
                }
            }
            return;
        }
        // The length may count up to three bytes of padding.
        for padding in 0..4 {
            let Some(end) = psdu.len().checked_sub(padding) else {
                break;
            };
            if fcs_ok(&psdu[..end]) {
                out.push((psdu[..end - 4].to_vec(), true));
                return;
            }
        }
        out.push((psdu.to_vec(), false));
    }
}

/// The MPDUs of an A-MPDU, up to the first damaged delimiter.
fn ampdu(data: &[u8]) -> Vec<&[u8]> {
    let mut mpdus = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let delimiter = &data[pos..pos + 4];
        if delimiter[3] != 0x4e {
            break;
        }
        let bits: Vec<u8> = (0..16).map(|b| (delimiter[b / 8] >> (b % 8)) & 1).collect();
        if crc8(&bits) != delimiter[2] {
            break;
        }
        let high = ((delimiter[0] >> 2) & 0x03) as usize;
        let low = (delimiter[0] >> 4) as usize | (delimiter[1] as usize) << 4;
        let len = high * 4096 + low;
        let start = pos + 4;
        if start + len > data.len() {
            break;
        }
        mpdus.push(&data[start..start + len]);
        pos = start + len.div_ceil(4) * 4;
    }
    mpdus
}

const FFT: usize = 128;
const DC: usize = FFT / 2;

/// FFT bin of the subcarrier at `offset` from DC.
const fn sc(offset: i32) -> usize {
    (DC as i32 + offset) as usize
}

/// Pilot subcarrier offsets.
const PILOT_OFFSETS: [i32; 4] = [-21, -7, 7, 21];
/// Pilot signs.
const PILOT_PSI: [f32; 4] = [1.0, 1.0, 1.0, -1.0];

/// Traveling pilot offsets (Table 23-22), one row per data symbol, cyclic.
const TRAVELING_PILOT_OFFSETS: [[i32; 4]; 14] = [
    [-28, -12, 4, 20],
    [-24, -8, 8, 24],
    [-20, -4, 12, 28],
    [-16, -2, 16, 26],
    [-26, -14, 2, 14],
    [-22, -10, 6, 18],
    [-18, -6, 10, 22],
    [-27, -11, 5, 21],
    [-23, -7, 9, 25],
    [-19, -3, 13, 23],
    [-15, 1, 17, 27],
    [-25, -13, -1, 11],
    [-21, -9, 3, 15],
    [-17, -5, 7, 19],
];

/// FFT bins of pilots at `offsets`, and of the data subcarriers between
/// -`edge` and `edge` around them.
const fn layout<const N: usize>(offsets: [i32; 4], edge: i32) -> ([usize; 4], [usize; N]) {
    let pilots = [
        sc(offsets[0]),
        sc(offsets[1]),
        sc(offsets[2]),
        sc(offsets[3]),
    ];
    let mut data = [0; N];
    let mut j = 0;
    let mut off = -edge;
    while off <= edge {
        let idx = sc(off);
        if off != 0 && idx != pilots[0] && idx != pilots[1] && idx != pilots[2] && idx != pilots[3]
        {
            data[j] = idx;
            j += 1;
        }
        off += 1;
    }
    assert!(j == N);
    (pilots, data)
}

/// Pilots and data subcarriers of data symbols with fixed pilots.
const FIXED: ([usize; 4], [usize; 52]) = layout(PILOT_OFFSETS, 28);

/// The same with traveling pilots, per row of `TRAVELING_PILOT_OFFSETS`.
const TRAVELING: [([usize; 4], [usize; 52]); 14] = {
    let mut rows = [([0; 4], [0; 52]); 14];
    let mut r = 0;
    while r < 14 {
        rows[r] = layout(TRAVELING_PILOT_OFFSETS[r], 28);
        r += 1;
    }
    rows
};

/// Data subcarriers of the signal field, which stays within ±26.
const SIG_DATA: [usize; 48] = layout(PILOT_OFFSETS, 26).1;

/// Order of the signal field's coded bits on its subcarriers, per symbol.
const SIG_INTERLEAVER: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

/// The long training field, per FFT bin.
fn ltf_freq() -> [Complex32; FFT] {
    let mut out = [Complex32::default(); FFT];
    let offsets = (-28..=28).filter(|&o| o != 0);
    for (off, &v) in offsets.zip(&LTF_AH) {
        out[sc(off)] = Complex32::new(v as f32, 0.0);
    }
    out
}

pub struct Equalizer {
    h: [Complex32; FFT],
    reference: [Complex32; FFT],
    snr: f32,
    sig: [[Complex32; 48]; 2],
    decoded: [u8; 48],
}

impl Equalizer {
    /// One hypothesis of the signal field's bits: CRC, then the fields.
    fn decode_candidate(
        &mut self,
        viterbi: &mut ViterbiDecoder,
        bits: &[u8; 96],
        long: bool,
    ) -> Option<FrameParam> {
        let mut deinterleaved = [0u8; 96];
        for sym in 0..2 {
            for (i, &k) in SIG_INTERLEAVER.iter().enumerate() {
                deinterleaved[sym * 48 + i] = bits[sym * 48 + k];
            }
        }
        viterbi.decode(CodeRate::R1_2, 2, 48, 48, &deinterleaved, &mut self.decoded);
        let s = &self.decoded;

        let crc = s[38..42].iter().fold(0, |c, &b| c << 1 | b);
        if crc4(&s[..38]) != crc {
            return None;
        }
        // No STBC, 2 MHz, one stream, BCC.
        if s[1] != 0 || s[3] | s[4] != 0 || s[5] | s[6] != 0 || s[17] != 0 {
            return None;
        }
        let mcs = s[19] | s[20] << 1 | s[21] << 2 | s[22] << 3;
        use CodeRate::*;
        use Modulation::*;
        let (modulation, rate) = match mcs {
            0 => (Bpsk, R1_2),
            1 => (Qpsk, R1_2),
            2 => (Qpsk, R3_4),
            3 => (Qam16, R1_2),
            4 => (Qam16, R3_4),
            5 => (Qam64, R2_3),
            6 => (Qam64, R3_4),
            7 => (Qam64, R5_6),
            _ => return None,
        };
        let mcs = Mcs::new(modulation, rate);
        let length = s[25..=33]
            .iter()
            .enumerate()
            .fold(0, |l, (i, &b)| l | (b as usize) << i);
        let mut frame = if s[24] > 0 {
            FrameParam::aggregate::<Ah>(mcs, length)
        } else {
            FrameParam::new::<Ah>(mcs, length)
        };
        frame.traveling_pilots = if long { s[37] > 0 } else { s[36] > 0 };
        frame.short_gi = s[16] > 0;
        // The long preamble has more training symbols after the signal
        // field.
        frame.skip_symbols = if long { 3 } else { 0 };
        frame.fits::<Ah>().then_some(frame)
    }
}

impl SymbolEqualizer for Equalizer {
    fn new() -> Self {
        Self {
            h: [Complex32::default(); FFT],
            reference: ltf_freq(),
            snr: 0.0,
            sig: [[Complex32::default(); 48]; 2],
            decoded: [0; 48],
        }
    }

    fn ltf(&mut self, k: usize, sym: &mut [Complex32]) {
        if k == 0 {
            self.h.copy_from_slice(sym);
            return;
        }
        let mut signal = 0.0f32;
        let mut noise = 0.0f32;
        for i in (sc(-28)..=sc(28)).filter(|&i| i != DC) {
            noise += (self.h[i] - sym[i]).norm_sqr();
            signal += (self.h[i] + sym[i]).norm_sqr();
            self.h[i] = (self.h[i] + sym[i]) / (self.reference[i] + self.reference[i]);
        }
        self.snr = 10.0 * (signal / noise / 2.0).log10();
    }

    fn signal(&mut self, k: usize, sym: &mut [Complex32], viterbi: &mut ViterbiDecoder) -> Signal {
        // Without pilot correction.
        let scale = (52.0f32 / 56.0).sqrt();
        for (o, &i) in SIG_DATA.iter().enumerate() {
            let h = self.h[i];
            self.sig[k][o] = if h.norm_sqr() > 0.0 {
                sym[i] / h * scale
            } else {
                Complex32::default()
            };
        }
        if k == 0 {
            return Signal::More;
        }
        // The short preamble's signal field is QBPSK in both symbols, the
        // long preamble's in the first only.
        let mut short = [0u8; 96];
        let mut long = [0u8; 96];
        for i in 0..48 {
            short[i] = (self.sig[0][i].im > 0.0) as u8;
            short[48 + i] = (self.sig[1][i].im > 0.0) as u8;
            long[i] = short[i];
            long[48 + i] = (self.sig[1][i].re > 0.0) as u8;
        }
        match self
            .decode_candidate(viterbi, &short, false)
            .or_else(|| self.decode_candidate(viterbi, &long, true))
        {
            Some(frame) => Signal::Frame(frame),
            None => Signal::Invalid,
        }
    }

    fn data(
        &mut self,
        frame: &FrameParam,
        n: usize,
        sym: &mut [Complex32],
        bits: &mut [u8],
        symbols: &mut [Complex32],
    ) {
        let (pilots, data) = if frame.traveling_pilots {
            &TRAVELING[n % 14]
        } else {
            &FIXED
        };
        let eq = |i: usize| {
            let h = self.h[i];
            if h.norm_sqr() > 0.0 {
                sym[i] / h
            } else {
                Complex32::default()
            }
        };
        let polarity = POLARITY[(n + 2) % 127] as f32;
        let mut acc = Complex32::default();
        for (k, &p) in pilots.iter().enumerate() {
            acc += eq(p) * polarity * PILOT_PSI[(k + n) % 4];
        }
        let rot = Complex32::from_polar(1.0, -acc.arg());
        let modulation = frame.mcs.modulation;
        for (o, &i) in data.iter().enumerate() {
            let v = eq(i) * rot;
            symbols[o] = v;
            bits[o] = modulation.demap(&v);
        }
    }

    fn channel(&self) -> Vec<Complex32> {
        (sc(-28)..=sc(28))
            .filter(|&i| i != DC)
            .map(|i| self.h[i])
            .collect()
    }

    fn snr(&self) -> f32 {
        self.snr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subcarrier_layouts() {
        let mut layouts = vec![FIXED];
        layouts.extend(TRAVELING);
        for (pilots, data) in layouts {
            let mut all: Vec<usize> = pilots.iter().chain(&data).copied().collect();
            all.sort();
            let want: Vec<usize> = (-28..=28).filter(|&o| o != 0).map(sc).collect();
            assert_eq!(all, want);
        }
        assert!(
            SIG_DATA
                .iter()
                .all(|&i| (sc(-26)..=sc(26)).contains(&i) && i != DC)
        );
        assert!(SIG_DATA.iter().all(|i| !FIXED.0.contains(i)));
    }

    #[test]
    fn ltf_taps_find_the_training_symbol() {
        // The matched filter peaks at the start of each repetition.
        let taps = Ah::ltf_taps();
        let ltf: Vec<Complex32> = taps.iter().map(|t| t.conj()).collect();
        let signal: Vec<Complex32> = ltf.iter().chain(&ltf).chain(&ltf).copied().collect();
        let cor: Vec<f32> = (0..2 * FFT)
            .map(|i| {
                let c: Complex32 = (0..FFT).map(|k| signal[i + k] * taps[k]).sum();
                c.norm()
            })
            .collect();
        for (lag, &c) in cor.iter().enumerate() {
            let near = [0, FFT, 2 * FFT].iter().any(|&p| lag.abs_diff(p) < 8);
            if !near {
                assert!(c < 0.5 * cor[0], "{lag}: {c} {}", cor[0]);
            }
        }
        assert!((cor[FFT] - cor[0]).abs() < 1e-2 * cor[0]);
    }

    fn delimiter(len: usize) -> [u8; 4] {
        let mut d = [
            ((len >> 12) as u8 & 3) << 2 | (len as u8) << 4,
            (len >> 4) as u8,
            0,
            0x4e,
        ];
        let bits: Vec<u8> = (0..16).map(|b| (d[b / 8] >> (b % 8)) & 1).collect();
        d[2] = crc8(&bits);
        d
    }

    #[test]
    fn ampdus_are_split() {
        let mut mpdu = b"a frame".to_vec();
        mpdu.extend(crate::crc32(&mpdu).to_le_bytes());
        let mut data = Vec::new();
        for m in [&mpdu[..], &b"xy"[..]] {
            data.extend(delimiter(m.len()));
            data.extend(m);
            data.resize(data.len().div_ceil(4) * 4, 0);
        }
        // An EOF padding delimiter, then a damaged one.
        data.extend(delimiter(0));
        data.extend([0x12, 0x34, 0x56, 0x4e]);
        assert_eq!(ampdu(&data), [&mpdu[..], &b"xy"[..], &b""[..]]);

        let frame = FrameParam {
            aggregation: true,
            ..FrameParam::new::<Ah>(Mcs::new(Modulation::Bpsk, CodeRate::R1_2), data.len())
        };
        let mut out = Vec::new();
        Ah::mpdus(&frame, &data, &mut out);
        let want = [
            (b"a frame".to_vec(), true),
            (b"xy".to_vec(), false),
            (Vec::new(), false),
        ];
        assert_eq!(out, want);
    }

    #[test]
    fn padding_after_the_fcs_is_found() {
        let mut psdu = b"another frame".to_vec();
        psdu.extend(crate::crc32(&psdu).to_le_bytes());
        let frame = FrameParam::new::<Ah>(Mcs::new(Modulation::Bpsk, CodeRate::R1_2), 0);
        for padding in 0..5 {
            let mut padded = psdu.clone();
            padded.resize(psdu.len() + padding, 0xa5);
            let mut out = Vec::new();
            Ah::mpdus(&frame, &padded, &mut out);
            if padding < 4 {
                assert_eq!(out, [(b"another frame".to_vec(), true)]);
            } else {
                assert_eq!(out, [(padded, false)]);
            }
        }
    }
}
