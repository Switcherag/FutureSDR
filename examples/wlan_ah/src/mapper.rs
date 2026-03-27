use futuresdr::prelude::*;

use crate::crc4;
use crate::data_sc_for_symbol;
use crate::pilot_sc_for_symbol;
use crate::sig_data_sc;
use crate::sc;
use crate::FrameParam;
use crate::Modulation;
use crate::FFT_SIZE;
use crate::N_DATA_SC;
use crate::N_SIG_DATA_SC;
use crate::PILOT_PSI;
use crate::POLARITY;

/// 802.11ah SIG field: 2 OFDM symbols × 48 data subcarriers = 96 coded bits.
/// 48 data bits: 38 info + 4 CRC-4 + 6 tail → rate 1/2 → 96 coded → interleaved.
struct Signal {
    signal: [u8; 48],
    signal_encoded: [u8; 96],
    signal_interleaved: [u8; 96],
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

    /// Generate 802.11ah S1G SIG field (48 bits).
    ///
    /// Layout (LSB-first):
    ///   bit  0:    reserved (0)
    ///   bit  1:    STBC (0)
    ///   bit  2:    uplink indication (0)
    ///   bits 3-4:  bandwidth (0 = 2 MHz)
    ///   bits 5-6:  NSTS (0 = 1 stream)
    ///   bits 7-15: ID (0)
    ///   bit  16:   short GI
    ///   bit  17:   coding (0 = BCC)
    ///   bit  18:   LDPC extra (0)
    ///   bits 19-22: MCS index (4 bits)
    ///   bit  23:   smoothing (0)
    ///   bit  24:   aggregation
    ///   bits 25-33: length (9 bits)
    ///   bits 34-35: response indication (0)
    ///   bit  36:   traveling pilots
    ///   bit  37:   NDP indication (0)
    ///   bits 38-41: CRC-4
    ///   bits 42-47: tail (0)
    fn generate_signal_field(&mut self, frame: &FrameParam) {
        self.signal.fill(0);

        // bit 0: reserved
        // bit 1: STBC = 0
        // bit 2: uplink indication = 0
        // bits 3-4: bandwidth = 0 (2 MHz)
        // bits 5-6: NSTS = 0 (1 stream)
        // bits 7-15: ID = 0
        // bit 16: short GI
        self.signal[16] = u8::from(frame.short_gi);
        // bit 17: coding = 0 (BCC)
        // bit 18: LDPC extra = 0
        // bits 19-22: MCS index
        let mcs_idx = frame.mcs().mcs_index();
        self.signal[19] = Self::get_bit(mcs_idx, 0);
        self.signal[20] = Self::get_bit(mcs_idx, 1);
        self.signal[21] = Self::get_bit(mcs_idx, 2);
        self.signal[22] = Self::get_bit(mcs_idx, 3);
        // bit 23: smoothing = 0
        // bit 24: aggregation
        self.signal[24] = u8::from(frame.aggregation);
        // bits 25-33: length (9 bits)
        let length = if frame.aggregation {
            frame.n_symbols()
        } else {
            frame.psdu_size()
        };
        for i in 0..9 {
            self.signal[25 + i] = Self::get_bit_usize(length, i);
        }
        // bits 34-35: response indication = 0
        // bit 36: traveling pilots
        self.signal[36] = u8::from(frame.traveling_pilots);
        // bit 37: NDP indication = 0

        // CRC-4 over bits 0..37 (MSB-first storage)
        let crc = crc4(&self.signal[0..38]);
        for i in 0..4 {
            self.signal[38 + i] = Self::get_bit(crc, 3 - i);
        }
        // bits 42-47: tail (already 0)

        // Convolutional encode (rate 1/2): 48 bits → 96 coded bits
        let mut state = 0u8;
        for i in 0..48 {
            state = ((state << 1) & 0x7e) | self.signal[i];
            self.signal_encoded[i * 2] = (state & 0o155).count_ones() as u8 % 2;
            self.signal_encoded[i * 2 + 1] = (state & 0o117).count_ones() as u8 % 2;
        }

        // Interleave each 48-bit symbol separately (same pattern as 802.11a/g SIG)
        const INTERLEAVER_PATTERN: [usize; 48] = [
            0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19,
            22, 25, 28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38,
            41, 44, 47,
        ];

        for sym in 0..2 {
            for i in 0..48 {
                self.signal_interleaved[sym * 48 + INTERLEAVER_PATTERN[i]] =
                    self.signal_encoded[sym * 48 + i];
            }
        }
    }
}

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
    traveling_pilots: bool,
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
                signal: [0; 48],
                signal_encoded: [0; 96],
                signal_interleaved: [0; 96],
            },
            current_mod: Modulation::Bpsk,
            current_len: 0,
            index: 0,
            traveling_pilots: false,
        }
    }

    /// Map a SIG OFDM symbol (48 data subcarriers, BPSK rotated onto imaginary axis).
    fn map_sig(input: &[u8], output: &mut [Complex32], polarity_index: usize) {
        debug_assert!(input.len() == N_SIG_DATA_SC);
        debug_assert!(output.len() == FFT_SIZE);

        output.fill(Complex32::new(0.0, 0.0));

        // Pilots at fixed positions
        let pilot_sc = [
            sc(crate::PILOT_OFFSETS[0]),
            sc(crate::PILOT_OFFSETS[1]),
            sc(crate::PILOT_OFFSETS[2]),
            sc(crate::PILOT_OFFSETS[3]),
        ];
        let pol = POLARITY[polarity_index % 127];
        for (p, &psi) in pilot_sc.iter().zip(PILOT_PSI.iter()) {
            // SIG pilots are on imaginary axis (BPSK rotated by π/2)
            output[*p] = Complex32::new(0.0, pol.re * psi);
        }

        // Data subcarriers (48, guard ±26, BPSK rotated onto imaginary axis)
        let data_sc = sig_data_sc();
        for (i, &sc_idx) in data_sc.iter().enumerate() {
            let bit_val = if input[i] > 0 { -1.0 } else { 1.0 };
            // Rotated BPSK: map onto imaginary axis
            output[sc_idx] = Complex32::new(0.0, bit_val);
        }
    }

    /// Map a data OFDM symbol (52 data subcarriers).
    fn map_data(
        input: &[u8],
        output: &mut [Complex32],
        modulation: Modulation,
        sym_index: usize,
        polarity_index: usize,
        traveling: bool,
    ) {
        debug_assert!(input.len() == N_DATA_SC);
        debug_assert!(output.len() == FFT_SIZE);

        output.fill(Complex32::new(0.0, 0.0));

        // Pilots
        let pilot_sc = pilot_sc_for_symbol(sym_index, traveling);
        let pol = POLARITY[polarity_index % 127];
        for (p, &psi) in pilot_sc.iter().zip(PILOT_PSI.iter()) {
            output[*p] = Complex32::new(pol.re * psi, 0.0);
        }

        // Data subcarriers
        let data_sc = data_sc_for_symbol(sym_index, traveling);
        for (i, &sc_idx) in data_sc.iter().enumerate() {
            output[sc_idx] = modulation.map(input[i]);
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
        if output_len < 2 * FFT_SIZE || input_len < N_DATA_SC {
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
                self.traveling_pilots = frame.traveling_pilots;

                // SIG symbol 1 (polarity index 0)
                Self::map_sig(
                    &self.signal.signal_interleaved[0..48],
                    (&mut output[0..FFT_SIZE]).try_into().unwrap(),
                    0,
                );
                // SIG symbol 2 (polarity index 1)
                Self::map_sig(
                    &self.signal.signal_interleaved[48..96],
                    (&mut output[FFT_SIZE..2 * FFT_SIZE]).try_into().unwrap(),
                    1,
                );
                o += 2;
                out_tags.add_tag(
                    0,
                    Tag::NamedUsize("wifi_start".to_string(), frame.n_symbols() + 2),
                );
                assert_eq!(self.index, self.current_len);
                self.current_mod = frame.mcs().modulation();
                self.current_len = frame.n_symbols();
                self.index = 0;
                input = &input[0..std::cmp::min(input_len, frame.n_symbols() * N_DATA_SC)];
            } else {
                assert!(*index <= (self.current_len - self.index) * N_DATA_SC);
                input = &input[0..*index];
            }
        }

        let n = std::cmp::min(input.len() / N_DATA_SC, (output.len() / FFT_SIZE) - o);

        for i in 0..n {
            Self::map_data(
                (&input[i * N_DATA_SC..(i + 1) * N_DATA_SC]).try_into().unwrap(),
                (&mut output[(i + o) * FFT_SIZE..(i + o + 1) * FFT_SIZE])
                    .try_into()
                    .unwrap(),
                self.current_mod,
                self.index,
                self.index + 2, // polarity index offset by SIG symbols
                self.traveling_pilots,
            );
            self.index += 1;
        }

        self.input.consume(n * N_DATA_SC);
        self.output.produce((n + o) * FFT_SIZE);

        if self.input.finished() && n == input_len / N_DATA_SC {
            io.finished = true;
        }

        Ok(())
    }
}
