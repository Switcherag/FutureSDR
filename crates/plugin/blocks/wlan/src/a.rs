//! IEEE 802.11a/g, 20 MHz: `examples/wlan`.

use futuresdr::num_complex::Complex32;

use crate::CodeRate;
use crate::FrameParam;
use crate::Mcs;
use crate::Modulation;
use crate::Signal;
use crate::Standard;
use crate::SymbolEqualizer;
use crate::ViterbiDecoder;
use crate::fcs_ok;
use crate::tables::LTF_A;
use crate::tables::LTF_A_MATCHED;
use crate::tables::POLARITY;

/// IEEE 802.11a/g.
pub struct A;

impl Standard for A {
    const NAME: &'static str = "802.11a";
    const FFT_SIZE: usize = 64;
    const CP_LEN: usize = 16;
    const STF_WARMUP: usize = 0;
    const MAX_SAMPLES: usize = 540 * 80;
    const LTF_SEARCH: usize = 320;
    const N_DATA_SC: usize = 48;
    const INTERLEAVER_COLUMNS: usize = 16;
    const SERVICE_BITS: usize = 16;

    type Equalizer = Equalizer;

    fn ltf_taps() -> Vec<Complex32> {
        LTF_A_MATCHED.to_vec()
    }

    fn mpdus(_frame: &FrameParam, psdu: &[u8], out: &mut Vec<(Vec<u8>, bool)>) {
        if fcs_ok(psdu) {
            out.push((psdu[..psdu.len() - 4].to_vec(), true));
        } else {
            out.push((psdu.to_vec(), false));
        }
    }
}

/// Pilot subcarriers; the third has the opposite sign.
const PILOTS: [usize; 4] = [11, 25, 39, 53];

/// Subcarriers 6 to 58 without DC and pilots.
const DATA: [usize; 48] = {
    let mut data = [0; 48];
    let (mut i, mut k) = (6, 0);
    while i <= 58 {
        if i != 11 && i != 25 && i != 32 && i != 39 && i != 53 {
            data[k] = i;
            k += 1;
        }
        i += 1;
    }
    data
};

/// Order of the signal field's coded bits on its subcarriers.
const SIGNAL_INTERLEAVER: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

fn rotate(sym: &mut [Complex32], beta: f32) {
    let r = Complex32::from_polar(1.0, -beta);
    for x in sym {
        *x *= r;
    }
}

/// Common phase of the pilots of a symbol with pilot polarity `p`.
fn pilot_phase(sym: &[Complex32], p: f32) -> f32 {
    (sym[PILOTS[0]] * p + sym[PILOTS[2]] * p + sym[PILOTS[1]] * p + sym[PILOTS[3]] * -p).arg()
}

pub struct Equalizer {
    h: [Complex32; 64],
    snr: f32,
    bits: [u8; 48],
    decoded: [u8; 24],
}

impl Equalizer {
    fn equalize(
        &self,
        sym: &[Complex32],
        modulation: Modulation,
        bits: &mut [u8],
        out: &mut [Complex32],
    ) {
        for (o, &i) in DATA.iter().enumerate() {
            out[o] = sym[i] / self.h[i];
            bits[o] = modulation.demap(&out[o]);
        }
    }

    fn decode_signal(&mut self, viterbi: &mut ViterbiDecoder) -> Option<FrameParam> {
        let mut deinterleaved = [0u8; 48];
        for (d, &k) in deinterleaved.iter_mut().zip(&SIGNAL_INTERLEAVER) {
            *d = self.bits[k];
        }
        viterbi.decode(CodeRate::R1_2, 1, 48, 24, &deinterleaved, &mut self.decoded);
        let bits = &self.decoded;

        let mut rate = 0;
        let mut bytes = 0;
        let mut parity = false;
        for (i, &b) in bits[..17].iter().enumerate() {
            parity ^= b > 0;
            if b > 0 && i < 4 {
                rate |= 1 << i;
            }
            if b > 0 && i > 4 {
                bytes |= 1 << (i - 5);
            }
        }
        if parity as u8 != bits[17] {
            return None;
        }
        use CodeRate::*;
        use Modulation::*;
        let (modulation, rate) = match rate {
            11 => (Bpsk, R1_2),
            15 => (Bpsk, R3_4),
            10 => (Qpsk, R1_2),
            14 => (Qpsk, R3_4),
            9 => (Qam16, R1_2),
            13 => (Qam16, R3_4),
            8 => (Qam64, R2_3),
            12 => (Qam64, R3_4),
            _ => return None,
        };
        let frame = FrameParam::new::<A>(Mcs::new(modulation, rate), bytes);
        frame.fits::<A>().then_some(frame)
    }
}

impl SymbolEqualizer for Equalizer {
    fn new() -> Self {
        Self {
            h: [Complex32::default(); 64],
            snr: 0.0,
            bits: [0; 48],
            decoded: [0; 24],
        }
    }

    fn ltf(&mut self, k: usize, sym: &mut [Complex32]) {
        let beta = (sym[11] - sym[25] + sym[39] + sym[53]).arg();
        rotate(sym, beta);
        if k == 0 {
            self.h.copy_from_slice(sym);
            return;
        }
        let mut signal = 0.0f32;
        let mut noise = 0.0f32;
        for i in (6..=58).filter(|&i| i != 32) {
            noise += (self.h[i] - sym[i]).norm_sqr();
            signal += (self.h[i] + sym[i]).norm_sqr();
            let l = Complex32::new(LTF_A[i] as f32, 0.0);
            self.h[i] += sym[i];
            self.h[i] /= l + l;
        }
        self.snr = 10.0 * (signal / noise / 2.0).log10();
    }

    fn signal(&mut self, _k: usize, sym: &mut [Complex32], viterbi: &mut ViterbiDecoder) -> Signal {
        rotate(sym, pilot_phase(sym, POLARITY[0] as f32));
        let mut symbols = [Complex32::default(); 48];
        let mut bits = [0; 48];
        self.equalize(sym, Modulation::Bpsk, &mut bits, &mut symbols);
        self.bits = bits;
        match self.decode_signal(viterbi) {
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
        rotate(sym, pilot_phase(sym, POLARITY[(n + 1) % 127] as f32));
        self.equalize(sym, frame.mcs.modulation, bits, symbols);
    }

    fn channel(&self) -> Vec<Complex32> {
        (6..=58).filter(|&i| i != 32).map(|i| self.h[i]).collect()
    }

    fn snr(&self) -> f32 {
        self.snr
    }
}
