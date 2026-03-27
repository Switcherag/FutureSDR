#![allow(clippy::needless_range_loop)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::neg_multiply)]

use futuresdr::prelude::*;

// ============================================================
// Constants
// ============================================================

const MAX_PAYLOAD_SIZE: usize = 1500;
const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28;

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
    pub fn modulation(&self) -> Modulation {
        match self {
            Mcs::Bpsk_1_2 | Mcs::Bpsk_3_4 => Modulation::Bpsk,
            Mcs::Qpsk_1_2 | Mcs::Qpsk_3_4 => Modulation::Qpsk,
            Mcs::Qam16_1_2 | Mcs::Qam16_3_4 => Modulation::Qam16,
            Mcs::Qam64_2_3 | Mcs::Qam64_3_4 => Modulation::Qam64,
        }
    }

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
        let bits = 16 + 8 * psdu_size + 6;
        let mut n_symbols = bits / mcs.n_dbps();
        if !bits.is_multiple_of(mcs.n_dbps()) {
            n_symbols += 1;
        }
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

    pub fn psdu_size(&self) -> usize { self.psdu_size }
    pub fn mcs(&self) -> Mcs { self.mcs }
    pub fn n_symbols(&self) -> usize { self.n_symbols }
}

// ============================================================
// POLARITY table
// ============================================================

const POLARITY: [Complex32; 127] = [
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
// Signal field
// ============================================================

struct Signal {
    signal: [u8; 24],
    signal_encoded: [u8; 48],
    signal_interleaved: [u8; 48],
}

impl Signal {
    #[inline(always)]
    fn get_bit(data: u8, bit: usize) -> u8 {
        u8::from(data & (1 << bit) > 0)
    }
    #[inline(always)]
    fn get_bit_usize(data: usize, bit: usize) -> u8 {
        u8::from(data & (1 << bit) > 0)
    }

    fn generate_signal_field(&mut self, frame: &FrameParam) {
        let length = frame.psdu_size();
        let rate = frame.mcs().rate_field();

        self.signal[0] = Self::get_bit(rate, 3);
        self.signal[1] = Self::get_bit(rate, 2);
        self.signal[2] = Self::get_bit(rate, 1);
        self.signal[3] = Self::get_bit(rate, 0);
        self.signal[4] = 0;
        self.signal[5] = Self::get_bit_usize(length, 0);
        self.signal[6] = Self::get_bit_usize(length, 1);
        self.signal[7] = Self::get_bit_usize(length, 2);
        self.signal[8] = Self::get_bit_usize(length, 3);
        self.signal[9] = Self::get_bit_usize(length, 4);
        self.signal[10] = Self::get_bit_usize(length, 5);
        self.signal[11] = Self::get_bit_usize(length, 6);
        self.signal[12] = Self::get_bit_usize(length, 7);
        self.signal[13] = Self::get_bit_usize(length, 8);
        self.signal[14] = Self::get_bit_usize(length, 9);
        self.signal[15] = Self::get_bit_usize(length, 10);
        self.signal[16] = Self::get_bit_usize(length, 11);
        let sum: u8 = self.signal[0..17].iter().sum();
        self.signal[17] = sum % 2;

        // encode
        let mut state = 0;
        for i in 0..24 {
            state = ((state << 1) & 0x7e) | self.signal[i];
            self.signal_encoded[i * 2] = (state & 0o155).count_ones() as u8 % 2;
            self.signal_encoded[i * 2 + 1] = (state & 0o117).count_ones() as u8 % 2;
        }

        // interleave
        const INTERLEAVER_PATTERN: [usize; 48] = [
            0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19,
            22, 25, 28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38,
            41, 44, 47,
        ];

        for i in 0..48 {
            self.signal_interleaved[INTERLEAVER_PATTERN[i]] = self.signal_encoded[i];
        }
    }
}

// ============================================================
// Mapper Block
// ============================================================

#[derive(Block)]
pub struct Mapper<I = DefaultCpuReader<u8>, O = DefaultCpuWriter<Complex32>>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    input: I,
    #[output]
    output: O,
    signal: Signal,
    current_mod: Modulation,
    current_len: usize,
    index: usize,
}

impl<I, O> Mapper<I, O>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            signal: Signal {
                signal: [0; 24],
                signal_encoded: [0; 48],
                signal_interleaved: [0; 48],
            },
            current_mod: Modulation::Bpsk,
            current_len: 0,
            index: 0,
        }
    }

    fn map(input: &[u8; 48], output: &mut [Complex32; 64], modulation: Modulation, index: usize) {
        // dc
        output[32] = Complex32::new(0.0, 0.0);
        // guard
        for i in (0..6).chain(59..64) {
            output[i] = Complex32::new(0.0, 0.0);
        }
        // pilots
        for i in [11, 25, 39] {
            output[i] = POLARITY[index % 127];
        }
        output[53] = -POLARITY[index % 127];
        // data
        for (i, c) in (6..11)
            .chain(12..25)
            .chain(26..32)
            .chain(33..39)
            .chain(40..53)
            .chain(54..59)
            .enumerate()
        {
            output[c] = modulation.map(input[i]);
        }
    }
}

impl<I, O> Default for Mapper<I, O>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I, O> Kernel for Mapper<I, O>
where
    I: CpuBufferReader<Item = u8>,
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _m: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let (mut input, in_tags) = self.input.slice_with_tags();
        let (output, mut out_tags) = self.output.slice_with_tags();
        let input_len = input.len();
        let output_len = output.len();
        if output_len < 128 || input_len < 48 {
            if self.input.finished() {
                io.finished = true;
            }
            return Ok(());
        }

        let mut o = 0;

        if let Some((index, frame)) = in_tags.iter().find_map(|x| match x {
            ItemTag {
                index,
                tag: Tag::NamedAny(n, any),
            } => {
                if n == "wifi_start" {
                    any.downcast_ref::<FrameParam>().map(|x| (index, x))
                } else {
                    None
                }
            }
            _ => None,
        }) {
            if *index == 0 {
                self.signal.generate_signal_field(frame);
                Self::map(
                    &self.signal.signal_interleaved,
                    (&mut output[0..64]).try_into().unwrap(),
                    Modulation::Bpsk,
                    0,
                );
                o += 1;
                out_tags.add_tag(
                    0,
                    Tag::NamedUsize("wifi_start".to_string(), frame.n_symbols() + 1),
                );
                assert_eq!(self.index, self.current_len);
                self.current_mod = frame.mcs().modulation();
                self.current_len = frame.n_symbols();
                self.index = 0;
                input = &input[0..std::cmp::min(input_len, frame.n_symbols() * 48)];
            } else {
                assert!(*index <= (self.current_len - self.index) * 48);
                input = &input[0..*index];
            }
        }

        let n = std::cmp::min(input.len() / 48, (output.len() / 64) - o);

        for i in 0..n {
            self.index += 1;
            Self::map(
                (&input[i * 48..(i + 1) * 48]).try_into().unwrap(),
                (&mut output[(i + o) * 64..(i + o + 1) * 64])
                    .try_into()
                    .unwrap(),
                self.current_mod,
                self.index,
            );
        }

        self.input.consume(n * 48);
        self.output.produce((n + o) * 64);

        if self.input.finished() && n == input_len / 48 {
            io.finished = true;
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "WlanMapper",
    description: "WLAN 802.11 OFDM subcarrier mapper",
    config: (),
    create: |_cfg, _id| {
        Mapper::<DefaultCpuReader<u8>, DefaultCpuWriter<Complex32>>::new()
    }
}
