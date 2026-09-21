//! Long training field synchronization: `examples/wlan`'s `SyncLong`, with
//! the sizes of a [`Standard`].

use std::marker::PhantomData;

use futuresdr::prelude::*;

use crate::Rotation;
use crate::Standard;

#[derive(Clone, Copy, Debug)]
enum State {
    /// Waiting for a frame.
    Broken,
    /// At a frame, with the short training field's frequency offset.
    Sync(f32),
    /// Symbols of a frame passed on, and the frequency offset.
    Copy(usize, f32),
}

/// Finds the long training field of each frame the detector tagged; passes
/// on its two symbols, then each symbol without its cyclic prefix, all
/// corrected by the fine frequency offset, the first tagged `wifi_start`
/// with the total offset.
#[derive(Block)]
pub struct SyncLong<S: Standard, I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    pub(crate) input: I,
    #[output]
    pub(crate) output: O,
    taps: Box<[Complex32]>,
    cor: Box<[Complex32]>,
    state: State,
    standard: PhantomData<fn() -> S>,
}

impl<S: Standard, I, O> SyncLong<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new() -> Self {
        let mut input = I::default();
        input.set_min_items(S::LTF_SEARCH + 2 * S::FFT_SIZE);
        let mut output = O::default();
        output.set_min_items(2 * S::FFT_SIZE);
        output.set_min_buffer_size_in_items(2 * S::FFT_SIZE);
        Self {
            input,
            output,
            taps: S::ltf_taps().into_boxed_slice(),
            cor: vec![Complex32::default(); S::LTF_SEARCH].into_boxed_slice(),
            state: State::Broken,
            standard: PhantomData,
        }
    }

    /// Start of the first training symbol in `input`, and the frequency
    /// offset: the phase between the two strongest correlations, which are
    /// a symbol apart.
    fn sync(taps: &[Complex32], cor: &mut [Complex32], input: &[Complex32]) -> (usize, f32) {
        for (i, c) in cor.iter_mut().enumerate() {
            let mut sum = Complex32::default();
            for (x, t) in input[i..i + taps.len()].iter().zip(taps) {
                sum += x * t;
            }
            *c = sum;
        }
        // The first two of a stable sort by decreasing energy.
        let (mut best, mut second) = ((0, f32::NAN), (0, f32::NAN));
        for (i, c) in cor.iter().enumerate() {
            let e = c.norm_sqr();
            if i == 0 || e.total_cmp(&best.1).is_gt() {
                second = best;
                best = (i, e);
            } else if i == 1 || e.total_cmp(&second.1).is_gt() {
                second = (i, e);
            }
        }
        let (first, second) = if best.0 < second.0 {
            (best.0, second.0)
        } else {
            (second.0, best.0)
        };
        let offset = (cor[first] * cor[second].conj()).arg() / S::FFT_SIZE as f32;
        (first, offset)
    }
}

impl<S: Standard, I, O> Default for SyncLong<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Standard, I, O> Kernel for SyncLong<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let n = S::FFT_SIZE;
        let symbol = S::SYMBOL_LEN;
        let (input, in_tags) = self.input.slice_with_tags();
        let input_len = input.len();
        let (out, mut out_tags) = self.output.slice_with_tags();

        let mut limit = input_len;
        let mut next_tag = None;
        let tag = in_tags.iter().find_map(|t| match &t.tag {
            Tag::NamedF32(name, f) if name == "wifi_start" => Some((t.index, *f)),
            _ => None,
        });
        if let Some((index, f)) = tag {
            if index == 0 {
                self.state = State::Sync(f);
            } else {
                limit = index;
                next_tag = Some(index);
                if limit < symbol {
                    self.input.consume(limit);
                    io.call_again = true;
                    return Ok(());
                }
            }
        }

        let needed = S::LTF_SEARCH + 2 * n;
        let consumed = match self.state {
            State::Broken => limit,
            State::Sync(f_short) if limit >= needed && out.len() >= 2 * n => {
                let window = S::LTF_SEARCH + n - 1;
                let (start, f) = Self::sync(&self.taps, &mut self.cor, &input[..window]);
                let mut rotation = Rotation::new(f);
                for (o, x) in out[..2 * n].iter_mut().zip(&input[start..]) {
                    *o = x * rotation.next();
                }
                out_tags.add_tag(0, Tag::NamedF32("wifi_start".to_string(), f_short + f));
                self.output.produce(2 * n);
                io.call_again = true;
                self.state = State::Copy(0, f);
                start + 2 * n
            }
            State::Sync(_) if next_tag.is_some() => {
                // The next frame starts before this one's training field
                // is complete.
                self.state = State::Broken;
                io.call_again = true;
                limit
            }
            State::Sync(_) => 0,
            State::Copy(copied, f) => {
                let syms = (limit / symbol).min(out.len() / n);
                for s in 0..syms {
                    let o = &mut out[s * n..(s + 1) * n];
                    let x = &input[s * symbol + S::CP_LEN..];
                    let t = (copied + s) * symbol + 2 * n + S::CP_LEN;
                    let mut rotation = Rotation::at(f, t);
                    for (o, x) in o.iter_mut().zip(x) {
                        *o = x * rotation.next();
                    }
                }
                self.output.produce(syms * n);
                self.state = State::Copy(copied + syms, f);
                syms * symbol
            }
        };
        self.input.consume(consumed);

        if let Some(index) = next_tag
            && index - consumed < symbol
        {
            io.call_again = true;
        }
        let left = input_len - consumed;
        let enough = match self.state {
            State::Broken => 1,
            State::Sync(_) => needed,
            State::Copy(..) => symbol,
        };
        if self.input.finished() && left < enough && !io.call_again {
            io.finished = true;
        }
        Ok(())
    }
}
