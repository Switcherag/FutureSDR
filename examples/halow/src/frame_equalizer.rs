use futuresdr::prelude::*;

use crate::FrameParam;
use crate::LONG;
use crate::Mcs;
use crate::Modulation;
use crate::POLARITY;
use crate::SIG_PILOT_POLARITY;
use crate::ViterbiDecoder;
use crate::FFT_SIZE;
use crate::N_DATA_SC;
use crate::N_SIG_SYMBOLS;

/// 1 MHz interleaver pattern for 24 data subcarriers (Table 23-20)
/// 3 rows x 8 columns, column-major fill
const INTERLEAVER_PATTERN: [usize; 24] = [
    0, 3, 6, 9, 12, 15, 18, 21,
    1, 4, 7, 10, 13, 16, 19, 22,
    2, 5, 8, 11, 14, 17, 20, 23,
];

/// Active data subcarrier indices after FFT shift (centered)
/// Subcarriers +1..+6, +8..+13 (skip +7 pilot), -13..-8 (skip -7 pilot), -6..-1
/// In centered FFT notation: indices 1-6, 8-13, 19-24, 26-31
const DATA_SC_INDICES: [usize; 24] = [
    1, 2, 3, 4, 5, 6,       // subcarriers +1..+6
    8, 9, 10, 11, 12, 13,   // subcarriers +8..+13
    19, 20, 21, 22, 23, 24, // subcarriers -13..-8
    26, 27, 28, 29, 30, 31, // subcarriers -6..-1
];

/// Pilot subcarrier indices after FFT shift (centered)
/// +7 at index 7, -7 at index 25
const PILOT_POS: usize = 7;   // subcarrier +7
const PILOT_NEG: usize = 25;  // subcarrier -7

struct Equalizer {
    h: [Complex32; FFT_SIZE],
    snr: f32,
}

impl Equalizer {
    fn new() -> Self {
        Equalizer {
            h: [Complex32::new(0.0, 0.0); FFT_SIZE],
            snr: 0.0,
        }
    }
    fn sync1(&mut self, s: &[Complex32; FFT_SIZE]) {
        self.h.copy_from_slice(s);
    }
    fn sync2(&mut self, s: &[Complex32; FFT_SIZE]) {
        let mut signal = 0.0f32;
        let mut noise = 0.0f32;
        // Active subcarriers: 1-13 and 19-31 (skip DC=0 and guards 14-18)
        for i in (1..=13).chain(19..=31) {
            noise += (self.h[i] - s[i]).norm_sqr();
            signal += (self.h[i] + s[i]).norm_sqr();

            self.h[i] += s[i];
            self.h[i] /= LONG[i] + LONG[i];
        }
        self.snr = 10.0 * (signal / noise / 2.0).log10();
    }

    fn equalize(
        &mut self,
        input: &[Complex32; FFT_SIZE],
        output_symbols: &mut [Complex32; N_DATA_SC],
        output_bits: &mut [u8; N_DATA_SC],
        modulation: Modulation,
    ) {
        for (o, &idx) in DATA_SC_INDICES.iter().enumerate() {
            output_symbols[o] = input[idx] / self.h[idx];
            output_bits[o] = modulation.demap(&output_symbols[o]);
        }
    }

    fn snr(&self) -> f32 {
        self.snr
    }
}

#[derive(Debug)]
enum State {
    Sync1,
    Sync2,
    Signal(usize), // collecting SIG symbols, count of symbols collected so far
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
    sym_in: [Complex32; FFT_SIZE],
    sym_out: [Complex32; N_DATA_SC],
    decoded_bits: [u8; 48], // enough for SIG decoding
    bits_out: [u8; N_DATA_SC],
    decoder: ViterbiDecoder,
    syms: Vec<Complex32>,
    // SIG field: collect bits from all 6 OFDM symbols
    sig_bits: [[u8; N_DATA_SC]; N_SIG_SYMBOLS],
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
            sym_in: [Complex32::new(0.0, 0.0); FFT_SIZE],
            sym_out: [Complex32::new(0.0, 0.0); N_DATA_SC],
            decoded_bits: [0; 48],
            bits_out: [0; N_DATA_SC],
            decoder: ViterbiDecoder::new(),
            syms: Vec::new(),
            sig_bits: [[0; N_DATA_SC]; N_SIG_SYMBOLS],
        }
    }

    /// Decode the 1 MHz S1G SIG field from 6 OFDM symbols
    /// SIG is BPSK 1/2 with 2x repetition over 6 symbols (3 symbols per repetition)
    /// Each symbol has 24 bits → 6 × 24 = 144 bits total
    /// After combining repetitions: 72 coded bits → Viterbi 1/2 → 36 data bits
    fn decode_signal_field(
        decoder: &mut ViterbiDecoder,
        sig_bits: &[[u8; N_DATA_SC]; N_SIG_SYMBOLS],
        decoded_bits: &mut [u8; 48],
    ) -> Option<FrameParam> {
        // Combine the 6 SIG symbols into a single bit stream
        // First repetition: symbols 0-2 (72 coded bits)
        // Second repetition: symbols 3-5 (72 coded bits, XOR with scramble for MCS10)
        // For now, use first repetition only (can be improved with combining)
        let mut sig_coded = [0u8; 72]; // 3 symbols × 24 bits
        for sym in 0..3 {
            sig_coded[sym * N_DATA_SC..(sym + 1) * N_DATA_SC]
                .copy_from_slice(&sig_bits[sym]);
        }

        // Deinterleave (1 MHz interleaver, applied per-symbol)
        let mut deinterleaved = [0u8; 72];
        for sym in 0..3 {
            for i in 0..N_DATA_SC {
                deinterleaved[sym * N_DATA_SC + i] =
                    sig_coded[sym * N_DATA_SC + INTERLEAVER_PATTERN[i]];
            }
        }

        // Viterbi decode: 72 coded bits → 36 data bits (rate 1/2)
        decoder.decode(
            FrameParam::new(Mcs::Mcs0, 0),
            &deinterleaved,
            decoded_bits,
        );

        // Extract SIG field contents
        // Bits 0-3: MCS index (4 bits)
        let mut mcs_idx = 0u8;
        for i in 0..4 {
            if decoded_bits[i] > 0 {
                mcs_idx |= 1 << i;
            }
        }

        // Bits 5-13: LENGTH (9 bits for 1 MHz, PSDU length in bytes)
        let mut length = 0usize;
        for i in 5..14 {
            if decoded_bits[i] > 0 {
                length |= 1 << (i - 5);
            }
        }

        // Bit 4: reserved
        // Bits 14-17: additional SIG info (varies)

        // Parity check over bits 0-17
        let mut parity = false;
        for i in 0..18 {
            parity ^= decoded_bits[i] > 0;
        }

        info!(
            "SIG field: MCS={}, length={}, parity={}",
            mcs_idx, length, parity
        );

        if let Some(mcs) = Mcs::from_index(mcs_idx) {
            if length > 0 && length <= crate::MAX_PSDU_SIZE {
                return Some(FrameParam::new(mcs, length));
            }
        }

        info!("signal: invalid MCS {} or length {}", mcs_idx, length);
        None
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
                if n == "halow_start" {
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

        let max_i = input.len() / FFT_SIZE;
        let max_o = out.len() / N_DATA_SC;
        let mut i = 0;
        let mut o = 0;

        while i < max_i {
            // FFT shift for 32-point: swap halves
            for k in 0..FFT_SIZE {
                let m = (k + FFT_SIZE / 2) % FFT_SIZE;
                self.sym_in[m] = input[i * FFT_SIZE + k];
            }

            // Pilot-based phase correction
            match self.state {
                State::Sync1 | State::Sync2 => {
                    // During LTF, use pilot positions for coarse phase
                    let beta =
                        (self.sym_in[PILOT_NEG] - self.sym_in[PILOT_POS]).arg();
                    for j in 0..FFT_SIZE {
                        self.sym_in[j] *= Complex32::from_polar(1.0, -beta);
                    }
                }
                State::Signal(sig_count) => {
                    // SIG field: use 1 MHz pilot polarity table
                    let (pol_neg, pol_pos) = SIG_PILOT_POLARITY[sig_count];
                    let beta = ((self.sym_in[PILOT_NEG] * Complex32::new(pol_neg, 0.0))
                        + (self.sym_in[PILOT_POS] * Complex32::new(pol_pos, 0.0)))
                        .arg();
                    for j in 0..FFT_SIZE {
                        self.sym_in[j] *= Complex32::from_polar(1.0, -beta);
                    }
                }
                State::Copy(left, n, _) => {
                    // Data symbols: use standard polarity sequence
                    let p = POLARITY[(n - left + N_SIG_SYMBOLS) % 127];
                    let beta = ((self.sym_in[PILOT_NEG] * -p)
                        + (self.sym_in[PILOT_POS] * p))
                        .arg();
                    for j in 0..FFT_SIZE {
                        self.sym_in[j] *= Complex32::from_polar(1.0, -beta);
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
                    self.state = State::Signal(0);
                    i += 1;
                }
                State::Signal(sig_count) => {
                    // Equalize and collect SIG bits
                    self.equalizer.equalize(
                        &self.sym_in,
                        &mut self.sym_out,
                        &mut self.bits_out,
                        Modulation::Bpsk,
                    );
                    self.sig_bits[*sig_count] = self.bits_out;
                    i += 1;

                    if *sig_count + 1 == N_SIG_SYMBOLS {
                        // All 6 SIG symbols collected, decode
                        if let Some(frame) = Self::decode_signal_field(
                            &mut self.decoder,
                            &self.sig_bits,
                            &mut self.decoded_bits,
                        ) {
                            info!(
                                "SIG decoded: {:?}, snr {}",
                                &frame,
                                self.equalizer.snr()
                            );

                            self.state = State::Copy(
                                frame.n_symbols(),
                                frame.n_symbols(),
                                frame.mcs().modulation(),
                            );
                            out_tags.add_tag(
                                o * N_DATA_SC,
                                Tag::NamedAny("halow_start".to_string(), Box::new(frame)),
                            );
                        } else {
                            info!(
                                "SIG field could not be decoded, snr {}",
                                self.equalizer.snr()
                            );
                            self.state = State::Skip;
                        }
                    } else {
                        self.state = State::Signal(*sig_count + 1);
                    }
                }
                &mut State::Copy(mut n_sym, ref mut all_sym, ref mut modulation) => {
                    if o < max_o {
                        self.equalizer.equalize(
                            &self.sym_in,
                            &mut self.sym_out,
                            (&mut out[o * N_DATA_SC..(o + 1) * N_DATA_SC])
                                .try_into()
                                .unwrap(),
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

        self.input.consume(i * FFT_SIZE);
        self.output.produce(o * N_DATA_SC);

        if self.input.finished() && i == max_i {
            io.finished = true;
        }

        Ok(())
    }
}
