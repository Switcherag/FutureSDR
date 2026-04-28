use futuresdr::prelude::*;

use crate::{long_time_domain, FFT_SIZE, CP_LEN, SYMBOL_LEN};

const SEARCH_WINDOW: usize = 2 * SYMBOL_LEN;

#[derive(Debug)]
enum State {
    Broken,
    Sync(f32),
    Copy(usize, f32),
}

struct Correlator {
    long_td: Vec<Complex32>,
    cor: Vec<Complex32>,
    cor_index: Vec<(usize, f32)>,
}

impl Correlator {
    fn new() -> Self {
        Correlator {
            long_td: long_time_domain(),
            cor: vec![Complex32::new(0.0, 0.0); SEARCH_WINDOW],
            cor_index: Vec::with_capacity(SEARCH_WINDOW),
        }
    }

    fn sync(&mut self, input: &[Complex32]) -> (usize, f32) {
        debug_assert!(input.len() >= SEARCH_WINDOW + FFT_SIZE - 1);

        for i in 0..SEARCH_WINDOW {
            unsafe {
                let mut sum = Complex32::new(0.0, 0.0);
                for k in 0..FFT_SIZE {
                    sum += *input.get_unchecked(i + k) * *self.long_td.get_unchecked(k);
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
#[message_outputs(corr_mag, sync_info, ltf_td)]
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
            corr: Correlator::new(),
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
        mio: &mut MessageOutputs,
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
                if n == "wifi_start" {
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
                if m < SYMBOL_LEN {
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
                if m >= SEARCH_WINDOW + 2 * FFT_SIZE {
                    let (offset, freq_offset) =
                        self.corr.sync(&input[0..SEARCH_WINDOW + FFT_SIZE - 1]);

                    // ── Diagnostics ────────────────────────────────────
                    // 1. Correlation magnitude landscape (entire search window)
                    let mag: Vec<f32> = self.corr.cor.iter().map(|c| c.norm()).collect();
                    // 2. Top-two peaks (sorted by index, same logic as sync())
                    let first = self.corr.cor_index[0].0.min(self.corr.cor_index[1].0);
                    let second = self.corr.cor_index[0].0.max(self.corr.cor_index[1].0);
                    let first_mag = self.corr.cor[first].norm();
                    let second_mag = self.corr.cor[second].norm();
                    let gap = (second as i32 - first as i32) as f32;
                    let info = vec![
                        first as f32,
                        second as f32,
                        gap,
                        freq_offset,
                        first_mag,
                        second_mag,
                    ];
                    println!(
                        "SYNC_LONG: peak1={} peak2={} gap={} (expected 64) cfo={:.6} mag1={:.3} mag2={:.3}",
                        first, second, gap as i32, freq_offset, first_mag, second_mag
                    );
                    // 3. Time-domain LTF (160 samples starting at sync offset, pre-CFO-correction)
                    let ltf_td: Vec<Complex32> =
                        input[offset..offset + 2 * FFT_SIZE].to_vec();
                    mio.post("corr_mag", Pmt::VecF32(mag)).await?;
                    mio.post("sync_info", Pmt::VecF32(info)).await?;
                    mio.post("ltf_td", Pmt::VecCF32(ltf_td)).await?;

                    // Output 2 × FFT_SIZE samples (two LTF symbols for channel estimation)
                    for i in 0..(2 * FFT_SIZE) {
                        out[i] =
                            input[offset + i] * Complex32::from_polar(1.0, i as f32 * freq_offset);
                    }
                    out_tags.add_tag(
                        0,
                        Tag::NamedF32("wifi_start".to_string(), freq_offset_short + freq_offset),
                    );

                    self.input.consume(offset + 2 * FFT_SIZE);
                    self.output.produce(2 * FFT_SIZE);
                    io.call_again = true;

                    self.state = State::Copy(0, freq_offset);
                }
            }
            State::Copy(n_copied, freq_offset) => {
                let syms = m / SYMBOL_LEN;
                for i in 0..syms {
                    for k in 0..FFT_SIZE {
                        out[i * FFT_SIZE + k] = input[i * SYMBOL_LEN + CP_LEN + k]
                            * Complex32::from_polar(
                                1.0,
                                (n_copied + i * SYMBOL_LEN + 2 * FFT_SIZE + CP_LEN + k) as f32
                                    * freq_offset,
                            );
                    }
                }
                self.input.consume(syms * SYMBOL_LEN);
                self.output.produce(syms * FFT_SIZE);
                self.state = State::Copy(n_copied + syms * SYMBOL_LEN, freq_offset);
            }
        }

        if self.input.finished() && input_len - m < SYMBOL_LEN {
            io.finished = true;
        }

        Ok(())
    }
}
