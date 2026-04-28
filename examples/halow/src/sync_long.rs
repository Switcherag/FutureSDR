use futuresdr::prelude::*;

use crate::FFT_SIZE;
use crate::GI_SAMPLES;
use crate::SYMBOL_SAMPLES;

const SEARCH_WINDOW: usize = 320;

#[derive(Debug)]
enum State {
    Broken,
    Sync(f32),
    Copy(usize, f32),
}

struct Correlator {
    cor: [Complex32; SEARCH_WINDOW],
    cor_index: Vec<(usize, f32)>,
}

impl Correlator {
    fn sync(&mut self, input: &[Complex32]) -> (usize, f32) {
        debug_assert!(input.len() >= SEARCH_WINDOW + FFT_SIZE - 1);

        for i in 0..SEARCH_WINDOW {
            unsafe {
                let mut sum = Complex32::new(0.0, 0.0);
                for k in 0..FFT_SIZE {
                    sum += *input.get_unchecked(i + k) * *LONG.get_unchecked(k);
                }
                *self.cor.get_unchecked_mut(i) = sum;
            }
        }

        self.cor_index = self.cor.iter().map(|x| x.norm_sqr()).enumerate().collect();
        self.cor_index.sort_by(|x, y| y.1.total_cmp(&x.1));
        let (first, second) = if self.cor_index[0].0 < self.cor_index[1].0 {
            (self.cor_index[0].0, self.cor_index[1].0)
        } else {
            (self.cor_index[1].0, self.cor_index[0].0)
        };

        (
            first,
            (self.cor[first] * self.cor[second].conj()).arg() / FFT_SIZE as f32,
        )
    }
}

#[derive(Block)]
pub struct SyncLong<I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    input: I,
    #[output]
    output: O,
    corr: Correlator,
    state: State,
}

impl<I, O> SyncLong<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            corr: Correlator {
                cor: [Complex32::new(0.0, 0.0); SEARCH_WINDOW],
                cor_index: Vec::with_capacity(SEARCH_WINDOW),
            },
            state: State::Broken,
        }
    }
}

impl<I, O> Default for SyncLong<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I, O> Kernel for SyncLong<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _m: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let (input, in_tags) = self.input.slice_with_tags();
        let input_len = input.len();
        let (out, mut out_tags) = self.output.slice_with_tags();

        let mut m = std::cmp::min(input.len(), out.len());

        if let Some((index, freq)) = in_tags.iter().find_map(|x| match x {
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
                self.state = State::Sync(*freq);
            } else {
                m = std::cmp::min(m, *index);
                if m < SYMBOL_SAMPLES {
                    self.input.consume(m);
                    return Ok(());
                }
            }
        }

        match self.state {
            State::Broken => {
                if m > 0 {
                    panic!("Sync Long is in broken state")
                }
            }
            State::Sync(freq_offset_short) => {
                // Need SEARCH_WINDOW + 2*FFT_SIZE samples for correlation + 2 LTF symbols
                if m >= SEARCH_WINDOW + 2 * FFT_SIZE {
                    let (offset, freq_offset) =
                        self.corr.sync(&input[0..SEARCH_WINDOW + FFT_SIZE - 1]);

                    // Copy 2 LTF symbols (each FFT_SIZE=32 samples)
                    for i in 0..(2 * FFT_SIZE) {
                        out[i] =
                            input[offset + i] * Complex32::from_polar(1.0, i as f32 * freq_offset);
                    }
                    out_tags.add_tag(
                        0,
                        Tag::NamedF32(
                            "halow_start".to_string(),
                            freq_offset_short + freq_offset,
                        ),
                    );

                    self.input.consume(offset + 2 * FFT_SIZE);
                    self.output.produce(2 * FFT_SIZE);
                    io.call_again = true;

                    self.state = State::Copy(0, freq_offset);
                }
            }
            State::Copy(n_copied, freq_offset) => {
                // Extract FFT_SIZE samples from each SYMBOL_SAMPLES-sample OFDM symbol
                // (skip GI_SAMPLES guard interval)
                let syms = m / SYMBOL_SAMPLES;
                for i in 0..syms {
                    for k in 0..FFT_SIZE {
                        out[i * FFT_SIZE + k] = input[i * SYMBOL_SAMPLES + GI_SAMPLES + k]
                            * Complex32::from_polar(
                                1.0,
                                ((n_copied + i) * SYMBOL_SAMPLES
                                    + 2 * FFT_SIZE
                                    + GI_SAMPLES
                                    + k) as f32
                                    * freq_offset,
                            );
                    }
                }
                self.input.consume(syms * SYMBOL_SAMPLES);
                self.output.produce(syms * FFT_SIZE);
                self.state = State::Copy(n_copied + syms * SYMBOL_SAMPLES, freq_offset);
            }
        }

        if self.input.finished() && input_len - m < SYMBOL_SAMPLES {
            io.finished = true;
        }

        Ok(())
    }
}

/// Time-domain LTF correlation kernel for 1 MHz HaLow
/// This is the IFFT of the frequency-domain LONG values from lib.rs
/// Computed from IEEE 802.11ah-2016 Table 23-7 S1G LTF for 1 MHz
const LONG: [Complex32; 32] = [
    Complex32::new(0.8125, 0.0000),
    Complex32::new(-0.1526, 0.6498),
    Complex32::new(0.1554, 0.5765),
    Complex32::new(0.5765, -0.3830),
    Complex32::new(0.0310, -0.1247),
    Complex32::new(0.2819, 0.4633),
    Complex32::new(-0.5997, 0.2424),
    Complex32::new(-0.1692, 0.5312),
    Complex32::new(0.5000, 0.1250),
    Complex32::new(0.2617, -0.0253),
    Complex32::new(0.0030, 0.5884),
    Complex32::new(-0.6942, 0.2259),
    Complex32::new(0.1202, 0.2871),
    Complex32::new(0.2988, 0.0641),
    Complex32::new(-0.0972, -0.8204),
    Complex32::new(0.6091, 0.0312),
    Complex32::new(-0.8125, 0.0000),
    Complex32::new(0.6091, -0.0312),
    Complex32::new(-0.0972, 0.8204),
    Complex32::new(0.2988, -0.0641),
    Complex32::new(0.1202, -0.2871),
    Complex32::new(-0.6942, -0.2259),
    Complex32::new(0.0030, -0.5884),
    Complex32::new(0.2617, 0.0253),
    Complex32::new(0.5000, -0.1250),
    Complex32::new(-0.1692, -0.5312),
    Complex32::new(-0.5997, -0.2424),
    Complex32::new(0.2819, -0.4633),
    Complex32::new(0.0310, 0.1247),
    Complex32::new(0.5765, 0.3830),
    Complex32::new(0.1554, -0.5765),
    Complex32::new(-0.1526, -0.6498),
];
