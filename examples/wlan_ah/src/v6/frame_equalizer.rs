//! Fork of `examples/wlan/src/frame_equalizer.rs`, carrying 11a's state
//! machine over to S1G.
//!
//! Structure is unchanged from the 11a block: `Sync1`/`Sync2` build the
//! channel estimate from the two long-training symbols, `Signal` decodes the
//! rate/length header, `Copy` equalises and demaps the data symbols, `Skip`
//! idles. What changes is the standard underneath:
//!
//!   * 48 data subcarriers in a 64-point FFT become 52 in a 128-point FFT
//!   * the one-symbol 11a SIGNAL field with its parity bit becomes the
//!     two-symbol S1G-SIG-A with a CRC-4, so `Signal` spans two symbols
//!   * pilots carry the S1G `PILOT_PSI` signs and may travel between symbols
//!
//! The pilot correction stays 11a's: a single common-phase term from the four
//! pilots, with no slope fit. That is a deliberate difference from v5, which
//! carries v2's per-symbol polyfit tracker — the two designs are what this
//! receiver exists to compare.

use futuresdr::prelude::*;

use crate::v6::helpers::ltf_freq;
use crate::{
    DC_INDEX, FFT_SIZE, FrameParam, MAX_PSDU_SIZE, MAX_SYM, Mcs, Modulation, N_DATA_SC,
    N_SIG_DATA_SC, PILOT_PSI, POLARITY, ViterbiDecoder, crc4, data_sc_for_symbol,
    pilot_sc_for_symbol, sc, sig_data_sc,
};

const SIG_INTERLEAVER_PATTERN: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

/// Active subcarriers: offsets -28..=28 excluding DC.
fn active_range() -> (usize, usize) {
    (sc(-28), sc(28))
}

struct Equalizer {
    h: Vec<Complex32>,
    reference: [Complex32; FFT_SIZE],
    snr: f32,
}

impl Equalizer {
    fn new() -> Self {
        Self {
            h: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
            reference: ltf_freq(),
            snr: 0.0,
        }
    }

    fn sync1(&mut self, s: &[Complex32]) {
        self.h.copy_from_slice(&s[..FFT_SIZE]);
    }

    fn sync2(&mut self, s: &[Complex32]) {
        let (lo, hi) = active_range();
        let mut signal = 0.0f32;
        let mut noise = 0.0f32;
        for i in lo..=hi {
            if i == DC_INDEX {
                continue;
            }
            noise += (self.h[i] - s[i]).norm_sqr();
            signal += (self.h[i] + s[i]).norm_sqr();
            // reference is +/-1 real, so dividing by 2*ref is a sign flip.
            self.h[i] = (self.h[i] + s[i]) / (self.reference[i] + self.reference[i]);
        }
        self.snr = 10.0 * (signal / noise / 2.0).log10();
    }

    /// Equalize the SIG subcarriers of one SIG symbol.
    fn equalize_sig(&self, input: &[Complex32], out: &mut [Complex32]) {
        // SIG rides 48 of the 52 data subcarriers; renormalise for the
        // narrower occupancy so the slicer sees unit-scale points.
        let scale = (52.0f32 / 56.0).sqrt();
        for (o, &idx) in sig_data_sc().iter().enumerate() {
            let h = self.h[idx];
            out[o] = if h.norm_sqr() > 0.0 {
                input[idx] / h * scale
            } else {
                Complex32::new(0.0, 0.0)
            };
        }
    }

    /// Equalize, pilot-correct and demap one data symbol.
    fn equalize_data(
        &self,
        input: &[Complex32],
        nsym: usize,
        traveling: bool,
        modulation: Modulation,
        out_symbols: &mut [Complex32],
        out_bits: &mut [u8],
    ) {
        let mut eq = [Complex32::new(0.0, 0.0); FFT_SIZE];
        for k in 0..FFT_SIZE {
            if self.h[k].norm_sqr() > 0.0 {
                eq[k] = input[k] / self.h[k];
            }
        }

        // 11a's common-phase pilot correction, with S1G pilot signs.
        let pilots = pilot_sc_for_symbol(nsym, traveling);
        let pol = POLARITY[(nsym + 2) % 127];
        let mut acc = Complex32::new(0.0, 0.0);
        for (k, &p) in pilots.iter().enumerate() {
            let psi = PILOT_PSI[(k + nsym) % 4];
            acc += eq[p] * pol * Complex32::new(psi, 0.0);
        }
        let beta = acc.arg();
        let rot = Complex32::from_polar(1.0, -beta);

        for (o, &idx) in data_sc_for_symbol(nsym, traveling).iter().enumerate() {
            let v = eq[idx] * rot;
            out_symbols[o] = v;
            out_bits[o] = modulation.demap(&v);
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
    /// S1G-SIG-A spans two symbols; the payload is which one is next.
    Signal(usize),
    Copy(usize, usize, FrameParam),
    Skip,
}

#[derive(Block)]
#[message_outputs(symbols, channel_est)]
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
    sym_in: Vec<Complex32>,
    sym_out: Vec<Complex32>,
    sig_bits: [u8; 2 * N_SIG_DATA_SC],
    decoded_bits: [u8; 48],
    bits_out: Vec<u8>,
    decoder: ViterbiDecoder,
    syms: Vec<Complex32>,
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
            sym_in: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
            sym_out: vec![Complex32::new(0.0, 0.0); N_DATA_SC],
            sig_bits: [0; 2 * N_SIG_DATA_SC],
            decoded_bits: [0; 48],
            bits_out: vec![0; N_DATA_SC],
            decoder: ViterbiDecoder::new(),
            syms: Vec::new(),
        }
    }

    /// Viterbi + CRC-4 over one S1G-SIG-A bit hypothesis.
    fn decode_candidate(
        decoder: &mut ViterbiDecoder,
        scratch: &mut [u8; 48],
        sig_bits: &[u8; 96],
        is_long: bool,
    ) -> Option<FrameParam> {
        let mut deinterleaved = [0u8; 96];
        for sym in 0..2 {
            let off = sym * 48;
            for i in 0..48 {
                deinterleaved[off + i] = sig_bits[off + SIG_INTERLEAVER_PATTERN[i]];
            }
        }
        decoder.decode_raw(&deinterleaved, scratch, 48);

        let s = &*scratch;
        let mut crc_sig = 0u8;
        for i in 0..4 {
            if s[38 + i] > 0 {
                crc_sig |= 1 << (3 - i);
            }
        }
        if crc4(&s[0..38]) != crc_sig {
            return None;
        }
        // STBC, 2 MHz bandwidth, single stream, BCC coding.
        if s[1] != 0
            || (s[3] as u8) | ((s[4] as u8) << 1) != 0
            || (s[5] as u8) | ((s[6] as u8) << 1) != 0
            || s[17] != 0
        {
            return None;
        }

        let mcs_idx = (s[19] as u8)
            | ((s[20] as u8) << 1)
            | ((s[21] as u8) << 2)
            | ((s[22] as u8) << 3);
        let mcs = Mcs::from_mcs_index(mcs_idx)?;
        let mut length = 0usize;
        for (i, &b) in s[25..=33].iter().enumerate() {
            length |= (b as usize) << i;
        }
        let fp = FrameParam::with_options(
            mcs,
            length,
            s[24] > 0,
            if is_long { s[37] > 0 } else { s[36] > 0 },
            s[16] > 0,
            is_long,
        );
        if fp.n_symbols() > MAX_SYM || fp.psdu_size() > MAX_PSDU_SIZE {
            return None;
        }
        Some(fp)
    }

    fn decode_sig(
        decoder: &mut ViterbiDecoder,
        scratch: &mut [u8; 48],
        sig_eq: &[[Complex32; N_SIG_DATA_SC]; 2],
    ) -> Option<FrameParam> {
        let mut short_bits = [0u8; 96];
        let mut long_bits = [0u8; 96];
        for i in 0..48 {
            short_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
            short_bits[48 + i] = (sig_eq[1][i].im > 0.0) as u8;
            long_bits[i] = (sig_eq[0][i].im > 0.0) as u8;
            long_bits[48 + i] = (sig_eq[1][i].re > 0.0) as u8;
        }
        Self::decode_candidate(decoder, scratch, &short_bits, false)
            .or_else(|| Self::decode_candidate(decoder, scratch, &long_bits, true))
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
                if n == "wifi_start" {
                    Some((index, f))
                } else {
                    None
                }
            }
            _ => None,
        }) {
            if *index == 0 {
                if !matches!(self.state, State::Skip) {
                    // debug!, not info!: after an interband retune the front
                    // end takes ~4 ms to settle, and the STF detector latches
                    // repeatedly onto the transient during it — roughly 15
                    // cancels and CRC failures per swap, which buries the log
                    // at info level. Raise with RUST_LOG=debug when the
                    // detector's behaviour is what you are looking at.
                    debug!("v6 frame equalizer: canceling frame");
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
        let mut sig_eq = [[Complex32::new(0.0, 0.0); N_SIG_DATA_SC]; 2];

        while i < max_i {
            // FFT shift into DC-centred order.
            for k in 0..FFT_SIZE {
                let m = (k + DC_INDEX) % FFT_SIZE;
                self.sym_in[m] = input[i * FFT_SIZE + k];
            }

            match self.state {
                State::Sync1 => {
                    self.equalizer.sync1(&self.sym_in);
                    self.state = State::Sync2;
                    i += 1;
                }
                State::Sync2 => {
                    self.equalizer.sync2(&self.sym_in);
                    let (lo, hi) = active_range();
                    let h: Vec<Complex32> = (lo..=hi)
                        .filter(|k| *k != DC_INDEX)
                        .map(|k| self.equalizer.h[k])
                        .collect();
                    mio.post("channel_est", Pmt::VecCF32(h)).await?;
                    self.state = State::Signal(0);
                    i += 1;
                }
                State::Signal(which) => {
                    let mut buf = [Complex32::new(0.0, 0.0); N_SIG_DATA_SC];
                    self.equalizer.equalize_sig(&self.sym_in, &mut buf);
                    sig_eq[which] = buf;
                    for (n, v) in buf.iter().enumerate() {
                        self.sig_bits[which * N_SIG_DATA_SC + n] = (v.im > 0.0) as u8;
                    }
                    i += 1;

                    if which == 0 {
                        self.state = State::Signal(1);
                    } else {
                        match Self::decode_sig(
                            &mut self.decoder,
                            &mut self.decoded_bits,
                            &sig_eq,
                        ) {
                            Some(frame) => {
                                let extra = if frame.is_long { 3 } else { 0 };
                                self.state = State::Copy(extra, frame.n_symbols(), frame);
                            }
                            None => {
                                // See the note on the cancel log above: this
                                // fires in bursts on the post-retune transient,
                                // where a negative SNR means the detector
                                // latched onto settling, not onto signal.
                                debug!(
                                    "v6: SIG-A CRC failed, snr {:.1} dB",
                                    self.equalizer.snr()
                                );
                                self.state = State::Skip;
                            }
                        }
                    }
                }
                State::Copy(skip_left, left, ref frame) => {
                    // S1G_LONG prepends symbols the data path never reads.
                    if skip_left > 0 {
                        let f = frame.clone();
                        self.state = State::Copy(skip_left - 1, left, f);
                        i += 1;
                        continue;
                    }
                    if o >= max_o {
                        break;
                    }
                    let frame = frame.clone();
                    let nsym = frame.n_symbols() - left;
                    self.equalizer.equalize_data(
                        &self.sym_in,
                        nsym,
                        frame.traveling_pilots,
                        frame.mcs().modulation(),
                        &mut self.sym_out,
                        &mut self.bits_out,
                    );
                    if nsym == 0 {
                        out_tags.add_tag(
                            o * N_DATA_SC,
                            Tag::NamedAny("wifi_start".to_string(), Box::new(frame.clone())),
                        );
                    }
                    out[o * N_DATA_SC..(o + 1) * N_DATA_SC].copy_from_slice(&self.bits_out);
                    self.syms.extend_from_slice(&self.sym_out);

                    i += 1;
                    o += 1;

                    if left == 1 {
                        if !self.syms.is_empty() {
                            mio.post("symbols", Pmt::VecCF32(std::mem::take(&mut self.syms)))
                                .await?;
                        }
                        self.state = State::Skip;
                    } else {
                        self.state = State::Copy(0, left - 1, frame);
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
