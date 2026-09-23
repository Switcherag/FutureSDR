//! FFT and frame equalization: `examples/wlan`'s `Fft` and `FrameEqualizer`
//! in one block, the standard-specific parts behind [`SymbolEqualizer`].

use std::sync::Arc;

use futuresdr::fft;
use futuresdr::fft::Fft;
use futuresdr::prelude::*;

use crate::Deconvolve;
use crate::FrameParam;
use crate::Standard;
use crate::ViterbiDecoder;

/// What a signal field symbol told.
pub enum Signal {
    /// The field has more symbols.
    More,
    /// A frame follows.
    Frame(FrameParam),
    /// Not a signal field, or a frame the receiver cannot take.
    Invalid,
}

/// The standard-specific part of the equalizer. Symbols are in frequency
/// domain with DC in the middle, and may be changed in place.
pub trait SymbolEqualizer: Send + 'static {
    fn new() -> Self;
    /// Long training symbol `k` (0 or 1): estimate the channel.
    fn ltf(&mut self, k: usize, sym: &mut [Complex32]);
    /// Signal field symbol `k`.
    fn signal(&mut self, k: usize, sym: &mut [Complex32], viterbi: &mut ViterbiDecoder) -> Signal;
    /// Data symbol `n` of `frame`: the equalized data subcarriers into
    /// `symbols`, and their demapped bits into `bits`, one byte each.
    fn data(
        &mut self,
        frame: &FrameParam,
        n: usize,
        sym: &mut [Complex32],
        bits: &mut [u8],
        symbols: &mut [Complex32],
    );
    /// The channel estimate on the used subcarriers.
    fn channel(&self) -> Vec<Complex32>;
    /// Signal to noise ratio of the training symbols, in dB.
    fn snr(&self) -> f32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Between frames.
    Skip,
    /// At long training symbol `k`.
    Ltf(usize),
    /// At signal field symbol `k`.
    Signal(usize),
    /// Symbols left before the data.
    Gap(usize),
    /// At data symbol `n`.
    Data(usize),
}

/// Equalizes the symbols of each frame `SyncLong` passes on: channel
/// estimation on the training symbols, signal field decoding, then the
/// demapped data subcarriers, one byte each, tagged `wifi_start` with the
/// [`FrameParam`] at a frame's first. Posts the equalized data symbols of
/// each frame on `symbols` and the channel estimate on `channel_est`.
#[derive(Block)]
#[message_outputs(symbols, channel_est)]
pub struct FrameEqualizer<S: Standard, I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<u8>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    #[input]
    pub(crate) input: I,
    #[output]
    pub(crate) output: O,
    /// None when the input is already in frequency domain, DC in the middle
    /// (`examples/wlan`'s `Fft` block in front, with `fft_shift`).
    fft: Option<Transform>,
    sym: Vec<Complex32>,
    equalizer: S::Equalizer,
    viterbi: ViterbiDecoder,
    state: State,
    frame: Option<FrameParam>,
    pending_tag: bool,
    sym_out: Vec<Complex32>,
    symbols: Vec<Complex32>,
}

impl<S: Standard, I, O> FrameEqualizer<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    /// With the FFT built in.
    pub fn new() -> Self {
        Self::with_fft(true)
    }

    /// With the FFT built in, or behind a separate `Fft` block (`fft` false).
    pub fn with_fft(fft: bool) -> Self {
        let mut input = I::default();
        input.set_min_items(S::FFT_SIZE);
        let mut output = O::default();
        output.set_min_items(S::N_DATA_SC);
        output.set_min_buffer_size_in_items(S::N_DATA_SC);
        Self {
            input,
            output,
            fft: fft.then(|| Transform::new(S::FFT_SIZE)),
            sym: vec![Complex32::default(); S::FFT_SIZE],
            equalizer: S::Equalizer::new(),
            // Signal fields have at most two symbols of 48 bits.
            viterbi: ViterbiDecoder::new(96),
            state: State::Skip,
            frame: None,
            pending_tag: false,
            sym_out: vec![Complex32::default(); S::N_DATA_SC],
            symbols: Vec::new(),
        }
    }
}

/// A forward FFT and its buffers.
struct Transform {
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    buf: Vec<Complex32>,
}

impl Transform {
    fn new(n: usize) -> Self {
        let fft = fft::forward(n);
        Self {
            scratch: vec![Complex32::default(); fft.get_inplace_scratch_len()],
            fft,
            buf: vec![Complex32::default(); n],
        }
    }

    /// Transform `input` into `sym`, DC in the middle.
    fn run(&mut self, input: &[Complex32], sym: &mut [Complex32]) {
        let n = self.buf.len();
        self.buf.copy_from_slice(input);
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        let (low, high) = self.buf.split_at(n / 2);
        sym[..n / 2].copy_from_slice(high);
        sym[n / 2..].copy_from_slice(low);
    }
}

impl<S: Standard, I, O> Default for FrameEqualizer<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Standard, I, O> Kernel for FrameEqualizer<S, I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let n = S::FFT_SIZE;
        let nd = S::N_DATA_SC;
        let (input, in_tags) = self.input.slice_with_tags();
        let (out, mut out_tags) = self.output.slice_with_tags();

        let mut input = input;
        let mut next_tag = None;
        let tag = in_tags.iter().find_map(|t| match &t.tag {
            Tag::NamedF32(name, _) if name == "wifi_start" => Some(t.index),
            _ => None,
        });
        match tag {
            Some(0) => {
                if self.state != State::Skip {
                    debug!("{} equalizer: canceling frame", S::NAME);
                }
                self.state = State::Ltf(0);
                self.frame = None;
                self.symbols.clear();
            }
            Some(index) => {
                input = &input[..index];
                next_tag = Some(index);
            }
            None => {}
        }

        let max_i = input.len() / n;
        let max_o = out.len() / nd;
        let mut i = 0;
        let mut o = 0;
        while i < max_i {
            match self.state {
                State::Skip => {
                    i += 1;
                    continue;
                }
                State::Gap(left) => {
                    self.state = if left == 1 {
                        State::Data(0)
                    } else {
                        State::Gap(left - 1)
                    };
                    i += 1;
                    continue;
                }
                State::Data(_) if o == max_o => break,
                _ => {}
            }
            let symbol = &input[i * n..(i + 1) * n];
            match &mut self.fft {
                Some(fft) => fft.run(symbol, &mut self.sym),
                None => self.sym.copy_from_slice(symbol),
            }
            i += 1;
            match self.state {
                State::Ltf(k) => {
                    self.equalizer.ltf(k, &mut self.sym);
                    if k == 0 {
                        self.state = State::Ltf(1);
                    } else {
                        mo.post("channel_est", Pmt::VecCF32(self.equalizer.channel()))
                            .await?;
                        self.state = State::Signal(0);
                    }
                }
                State::Signal(k) => {
                    match self.equalizer.signal(k, &mut self.sym, &mut self.viterbi) {
                        Signal::More => self.state = State::Signal(k + 1),
                        Signal::Frame(frame) => {
                            self.state = match frame.skip_symbols {
                                0 => State::Data(0),
                                gap => State::Gap(gap),
                            };
                            self.frame = Some(frame);
                            self.pending_tag = true;
                        }
                        Signal::Invalid => {
                            debug!(
                                "{}: signal field could not be decoded, snr {}",
                                S::NAME,
                                self.equalizer.snr()
                            );
                            self.state = State::Skip;
                        }
                    }
                }
                State::Data(k) => {
                    let Some(frame) = self.frame.as_ref() else {
                        self.state = State::Skip;
                        continue;
                    };
                    if self.pending_tag {
                        self.pending_tag = false;
                        out_tags.add_tag(
                            o * nd,
                            Tag::NamedAny("wifi_start".to_string(), Box::new(frame.clone())),
                        );
                    }
                    self.equalizer.data(
                        frame,
                        k,
                        &mut self.sym,
                        &mut out[o * nd..(o + 1) * nd],
                        &mut self.sym_out,
                    );
                    self.symbols.extend_from_slice(&self.sym_out);
                    o += 1;
                    if k + 1 == frame.n_symbols {
                        let symbols = std::mem::take(&mut self.symbols);
                        mo.post("symbols", Pmt::VecCF32(symbols)).await?;
                        self.state = State::Skip;
                    } else {
                        self.state = State::Data(k + 1);
                    }
                }
                State::Skip | State::Gap(_) => unreachable!(),
            }
        }

        self.input.consume(i * n);
        self.output.produce(o * nd);
        if next_tag == Some(i * n) {
            io.call_again = true;
        }
        if self.input.finished() && next_tag.is_none() && i == max_i {
            io.finished = true;
        }
        Ok(())
    }
}
