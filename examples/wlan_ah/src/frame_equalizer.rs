use futuresdr::prelude::*;

use crate::FrameParam;
use crate::Mcs;
use crate::Modulation;
use crate::POLARITY;
use crate::ViterbiDecoder;
use crate::{crc4, FFT_SIZE, DC_INDEX, N_DATA_SC, N_SIG_DATA_SC, PILOT_OFFSETS, PILOT_PSI,
            LTF_FREQ, sc, sig_data_sc, data_sc_for_symbol, pilot_sc_for_symbol};

/// SIG field interleaver pattern (48 coded bits, Ncol=16).
const SIG_INTERLEAVER_PATTERN: [usize; 48] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 42, 45, 1, 4, 7, 10, 13, 16, 19, 22, 25,
    28, 31, 34, 37, 40, 43, 46, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47,
];

struct Equalizer {
    h: Vec<Complex32>,
    snr: f32,
}

impl Equalizer {
    fn new() -> Self {
        Equalizer {
            h: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
            snr: 0.0,
        }
    }

    fn sync1(&mut self, s: &[Complex32]) {
        self.h.copy_from_slice(&s[..FFT_SIZE]);
    }

    fn sync2(&mut self, s: &[Complex32]) {
        let mut signal = 0.0f32;
        let mut noise = 0.0f32;

        // Average the two LTF symbols and divide by known LTF values
        let mut ltf_k = 0;
        for off in -28i32..=28 {
            if off == 0 { continue; }
            let i = sc(off);
            noise += (self.h[i] - s[i]).norm_sqr();
            signal += (self.h[i] + s[i]).norm_sqr();

            let ltf_val = LTF_FREQ[ltf_k];
            self.h[i] = (self.h[i] + s[i]) / Complex32::new(2.0 * ltf_val, 0.0);
            ltf_k += 1;
        }
        self.snr = 10.0 * (signal / noise / 2.0).log10();
    }

    fn equalize(
        &self,
        input: &[Complex32],
        data_sc: &[usize],
        output_symbols: &mut [Complex32],
        output_bits: &mut [u8],
        modulation: Modulation,
    ) {
        for (o, &i) in data_sc.iter().enumerate() {
            output_symbols[o] = input[i] / self.h[i];
            output_bits[o] = modulation.demap(&output_symbols[o]);
        }
    }

    /// Equalize the SIG field (BPSK rotated on imaginary axis).
    fn equalize_sig(
        &self,
        input: &[Complex32],
        sig_data: &[usize; N_SIG_DATA_SC],
        output_bits: &mut [u8],
        use_real: bool,
    ) {
        for (o, &i) in sig_data.iter().enumerate() {
            let eq = input[i] / self.h[i];
            // SIG uses BPSK rotated: signal is on the imaginary axis (or real for SIG-A sym 2)
            let val = if use_real { eq.re } else { eq.im };
            output_bits[o] = if val > 0.0 { 1 } else { 0 };
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
    Sig1,
    Sig2,
    /// Copy(remaining, total, modulation, n_extra, traveling_pilots)
    Copy(usize, usize, Modulation, usize, bool),
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
    sig_bits: Vec<u8>,     // 2 × 48 = 96 SIG coded bits
    decoded_bits: [u8; 48], // SIG decoded bits (2 × 48 coded → 48 data via Viterbi)
    decoder: ViterbiDecoder,
    syms: Vec<Complex32>,
    // Cumulative polynomial pilot tracking (like Python's pilot_polyfit)
    accum_alpha: f32,  // accumulated slope (phase per subcarrier)
    accum_beta: f32,   // accumulated intercept (flat phase)
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
            sig_bits: vec![0u8; 2 * N_SIG_DATA_SC],
            decoded_bits: [0; 48],
            decoder: ViterbiDecoder::new(),
            syms: Vec::new(),
            accum_alpha: 0.0,
            accum_beta: 0.0,
        }
    }

    /// Decode the 802.11ah SIG field (2 OFDM symbols, BPSK rotated, CRC-4).
    fn decode_signal_field(
        decoder: &mut ViterbiDecoder,
        sig_bits: &[u8],          // 96 coded bits (2 × 48)
        decoded_bits: &mut [u8; 48],
    ) -> Option<FrameParam> {
        // Deinterleave each 48-bit symbol separately
        let mut deinterleaved = vec![0u8; 96];
        for sym in 0..2 {
            for i in 0..48 {
                deinterleaved[sym * 48 + i] = sig_bits[sym * 48 + SIG_INTERLEAVER_PATTERN[i]];
            }
        }

        // Viterbi decode: 96 coded bits → 48 data bits (rate 1/2)
        // Use a temporary FrameParam with 2 symbols of BPSK 1/2 on 48 subcarriers
        // n_cbps for SIG = 48, so n_symbols = 2, total coded = 96
        decoder.decode_raw(&deinterleaved, decoded_bits, 48);

        // CRC-4 check: bits 0..37 are data, bits 38..41 are CRC (MSB-first), bits 42..47 are tail
        let crc_calc = crc4(&decoded_bits[0..38]);
        let mut crc_sig: u8 = 0;
        for i in 0..4 {
            if decoded_bits[38 + i] > 0 {
                crc_sig |= 1 << (3 - i);
            }
        }
        if crc_calc != crc_sig {
            return None;
        }

        // Parse SIG field bits (all LSB-first)
        // Bit 0: reserved (S1G_SHORT) or MU/SU (S1G_LONG)
        let _reserved_or_mu = decoded_bits[0];
        let stbc = decoded_bits[1];
        if stbc != 0 { return None; } // STBC not supported
        let _uplink_indication = decoded_bits[2];
        let bw = (decoded_bits[3] as u8) | ((decoded_bits[4] as u8) << 1);
        if bw != 0 { return None; } // Only 2 MHz supported
        let nsts = (decoded_bits[5] as u8) | ((decoded_bits[6] as u8) << 1);
        if nsts != 0 { return None; } // Only 1 space-time stream

        // Bits 7-15: ID
        // Bit 16: short GI
        let short_gi = decoded_bits[16] > 0;
        // Bit 17: coding (0=BCC, 1=LDPC)
        let coding = decoded_bits[17];
        if coding != 0 { return None; } // LDPC not supported
        // Bit 18: LDPC extra
        // Bits 19-22: MCS index (4 bits)
        let mcs_idx = (decoded_bits[19] as u8)
            | ((decoded_bits[20] as u8) << 1)
            | ((decoded_bits[21] as u8) << 2)
            | ((decoded_bits[22] as u8) << 3);
        // Bit 23: smoothing/beam change
        // Bit 24: aggregation
        let aggregation = decoded_bits[24] > 0;
        // Bits 25-33: length (9 bits)
        let mut length: usize = 0;
        for i in 0..9 {
            if decoded_bits[25 + i] > 0 {
                length |= 1 << i;
            }
        }
        // Bits 34-35: response indication
        // Bit 36: traveling pilots (S1G_SHORT) or reserved (S1G_LONG)
        // Bit 37: NDP indication (S1G_SHORT) or traveling pilots (S1G_LONG)
        // Note: is_long detection happens after this function returns;
        //       we default to bit 36 (S1G_SHORT). The caller can re-read bit 37 if needed.
        let traveling_pilots = decoded_bits[36] > 0;

        let mcs = match Mcs::from_mcs_index(mcs_idx) {
            Some(m) => m,
            None => {
                info!("signal: unsupported MCS index {}", mcs_idx);
                return None;
            }
        };

        let frame = FrameParam::with_options(
            mcs,
            length,
            aggregation,
            traveling_pilots,
            short_gi,
            false, // is_long detection is done separately
        );

        if frame.n_symbols() > crate::MAX_SYM || frame.psdu_size() > crate::MAX_PSDU_SIZE {
            info!("signal: frame too large ({:?})", frame);
            return None;
        }

        Some(frame)
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

        let sig_data = sig_data_sc();
        let pilot_indices = [
            sc(PILOT_OFFSETS[0]),
            sc(PILOT_OFFSETS[1]),
            sc(PILOT_OFFSETS[2]),
            sc(PILOT_OFFSETS[3]),
        ];

        while i < max_i {
            // Copy symbol with fft shift
            for k in 0..FFT_SIZE {
                let m = (k + DC_INDEX) % FFT_SIZE;
                self.sym_in[m] = input[i * FFT_SIZE + k];
            }

            // Pilot-based phase correction
            match self.state {
                State::Sync1 | State::Sync2 => {
                    // LTF symbols: correct phase using known pilot values from LTF
                    // LTF pilot values at [-21,-7,+7,+21]: [+1,-1,+1,+1]
                    // (derived from LTF_FREQ at the pilot subcarrier positions)
                    let beta = (self.sym_in[pilot_indices[0]]          // LTF[-21] = +1
                              - self.sym_in[pilot_indices[1]]          // LTF[-7]  = -1
                              + self.sym_in[pilot_indices[2]]          // LTF[+7]  = +1
                              + self.sym_in[pilot_indices[3]])         // LTF[+21] = +1
                        .arg();
                    for j in 0..FFT_SIZE {
                        self.sym_in[j] *= Complex32::from_polar(1.0, -beta);
                    }
                }
                State::Sig1 | State::Sig2 => {
                    // SIG uses Q-BPSK (signal on imaginary axis). The standard
                    // pilot-based phase correction removes the j rotation and
                    // pushes the signal onto the real axis, but equalize_sig
                    // extracts the imaginary component. Skip pilot correction
                    // for SIG — right after LTF the residual CFO is negligible.
                }
                State::Copy(left, n, _, n_extra, traveling) => {
                    let sym_idx = n - left;
                    // Only apply pilot correction to data symbols (skip D-STF/D-LTF1/SIG-B for S1G_LONG)
                    if sym_idx >= n_extra {
                        let data_sym_idx = sym_idx - n_extra;
                        let p = POLARITY[(data_sym_idx + 2) % 127]; // +2 for the 2 SIG symbols
                        let pilots = pilot_sc_for_symbol(data_sym_idx, traveling);
                        // 802.11ah Eq. 23-36: ψ_{(m+n) mod 4} — rotate PSI by data symbol index
                        let psi = [
                            PILOT_PSI[(0 + data_sym_idx) % 4],
                            PILOT_PSI[(1 + data_sym_idx) % 4],
                            PILOT_PSI[(2 + data_sym_idx) % 4],
                            PILOT_PSI[(3 + data_sym_idx) % 4],
                        ];
                        // Use equalized pilots (divide by H first) for cleaner phase estimate
                        let h = &self.equalizer.h;
                        let w = [
                            self.sym_in[pilots[0]] / h[pilots[0]] * p * Complex32::new(psi[0], 0.0),
                            self.sym_in[pilots[1]] / h[pilots[1]] * p * Complex32::new(psi[1], 0.0),
                            self.sym_in[pilots[2]] / h[pilots[2]] * p * Complex32::new(psi[2], 0.0),
                            self.sym_in[pilots[3]] / h[pilots[3]] * p * Complex32::new(psi[3], 0.0),
                        ];
                        // Pilot positions (subcarrier offsets from DC)
                        let x: [f32; 4] = [
                            (pilots[0] as f32) - DC_INDEX as f32,
                            (pilots[1] as f32) - DC_INDEX as f32,
                            (pilots[2] as f32) - DC_INDEX as f32,
                            (pilots[3] as f32) - DC_INDEX as f32,
                        ];
                        let x_mean = (x[0] + x[1] + x[2] + x[3]) * 0.25;

                        // Cumulative polynomial pilot tracking (matches Python's pilot_polyfit):
                        // 1. Remove previous accumulated correction from wiped pilots
                        // 2. Fit residual (slope + intercept) to the corrected phases
                        // 3. Add residual to accumulated correction
                        let mut sum_xc_phi = 0.0f32;
                        let mut sum_xc2 = 0.0f32;
                        let prev_rot = Complex32::from_polar(1.0, -self.accum_beta);
                        let mut sum_residual = Complex32::new(0.0, 0.0);
                        for k in 0..4 {
                            let prev_correction = self.accum_alpha * (x[k] - x_mean) + self.accum_beta;
                            let w_residual = w[k] * Complex32::from_polar(1.0, -prev_correction);
                            sum_residual += w_residual;
                        }
                        let residual_beta = sum_residual.arg();
                        let residual_rot = Complex32::from_polar(1.0, -residual_beta);
                        for k in 0..4 {
                            let prev_correction = self.accum_alpha * (x[k] - x_mean) + self.accum_beta;
                            let w_residual = w[k] * Complex32::from_polar(1.0, -prev_correction) * residual_rot;
                            let xc = x[k] - x_mean;
                            sum_xc_phi += xc * w_residual.im;
                            sum_xc2 += xc * xc;
                        }
                        let residual_alpha = if sum_xc2 > 0.0 { sum_xc_phi / sum_xc2 } else { 0.0 };

                        // Update accumulated correction
                        self.accum_alpha += residual_alpha;
                        self.accum_beta += residual_beta;

                        // Apply accumulated linear phase correction
                        for j in 0..FFT_SIZE {
                            let off = (j as f32) - DC_INDEX as f32;
                            self.sym_in[j] *= Complex32::from_polar(1.0, -(self.accum_alpha * (off - x_mean) + self.accum_beta));
                        }
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

                    // Post channel estimate (active subcarriers -28..+28 excl DC)
                    {
                        let mut h_active = Vec::with_capacity(crate::N_ACTIVE_SC);
                        for off in -28i32..=28 {
                            if off == 0 { continue; }
                            h_active.push(self.equalizer.h[crate::sc(off)]);
                        }
                        mio.post("channel_est", Pmt::VecCF32(h_active)).await?;
                    }

                    self.state = State::Sig1;
                    i += 1;
                }
                State::Sig1 => {
                    // First SIG symbol: BPSK rotated (imaginary axis)
                    self.equalizer.equalize_sig(
                        &self.sym_in,
                        &sig_data,
                        &mut self.sig_bits[0..N_SIG_DATA_SC],
                        false, // extract imaginary
                    );
                    i += 1;
                    self.state = State::Sig2;
                }
                State::Sig2 => {
                    // Second SIG symbol: also imaginary for S1G_SHORT
                    // For S1G_LONG (SIG-A), symbol 2 would be real — detect this below
                    //
                    // Try S1G_SHORT first (both symbols on imaginary axis)
                    self.equalizer.equalize_sig(
                        &self.sym_in,
                        &sig_data,
                        &mut self.sig_bits[N_SIG_DATA_SC..],
                        false,
                    );

                    i += 1;

                    // Detect S1G_LONG: if symbol 2 has more energy on real axis than imaginary
                    let mut real_energy = 0.0f32;
                    let mut imag_energy = 0.0f32;
                    for &sc_idx in &sig_data {
                        let eq = self.sym_in[sc_idx] / self.equalizer.h[sc_idx];
                        real_energy += eq.re * eq.re;
                        imag_energy += eq.im * eq.im;
                    }
                    let is_long = real_energy > imag_energy;

                    if is_long {
                        // Re-extract symbol 2 from real axis
                        self.equalizer.equalize_sig(
                            &self.sym_in,
                            &sig_data,
                            &mut self.sig_bits[N_SIG_DATA_SC..],
                            true,
                        );
                    }

                    if let Some(mut frame) = Self::decode_signal_field(
                        &mut self.decoder,
                        &self.sig_bits,
                        &mut self.decoded_bits,
                    ) {
                        frame.is_long = is_long;
                        // For S1G_LONG, traveling pilots is at bit 37 (not bit 36)
                        if is_long {
                            frame.traveling_pilots = self.decoded_bits[37] > 0;
                        }
                        info!(
                            "SIG decoded: MCS {} {:?}, {} syms, {} bytes, agg={}, tp={}, long={}",
                            frame.mcs().mcs_index(),
                            frame.mcs(),
                            frame.n_symbols(),
                            frame.psdu_size(),
                            frame.aggregation,
                            frame.traveling_pilots,
                            frame.is_long,
                        );

                        let n_extra = if is_long { 3 } else { 0 }; // D-STF, D-LTF1, SIG-B
                        let total_symbols = frame.n_symbols() + n_extra;

                        let traveling = frame.traveling_pilots;
                        // Reset cumulative pilot tracking for new frame
                        self.accum_alpha = 0.0;
                        self.accum_beta = 0.0;
                        self.state = State::Copy(
                            total_symbols,
                            total_symbols,
                            frame.mcs().modulation(),
                            n_extra,
                            traveling,
                        );
                        out_tags.add_tag(
                            o * N_DATA_SC,
                            Tag::NamedAny("wifi_start".to_string(), Box::new(frame)),
                        );
                    } else {
                        info!(
                            "SIG could not be decoded, snr {}",
                            self.equalizer.snr()
                        );
                        self.state = State::Skip;
                    }
                }
                &mut State::Copy(mut n_sym, ref mut all_sym, ref mut modulation, n_extra, traveling) => {
                    let sym_idx = *all_sym - n_sym;

                    if sym_idx < n_extra {
                        // Skip D-STF, D-LTF1, SIG-B (S1G_LONG preamble symbols)
                        i += 1;
                        n_sym -= 1;
                        if n_sym == 0 {
                            self.syms.clear();
                            self.state = State::Skip;
                        } else {
                            self.state = State::Copy(n_sym, *all_sym, *modulation, n_extra, traveling);
                        }
                    } else if o < max_o {
                        let data_sym_idx = sym_idx - n_extra;
                        let data_sc = data_sc_for_symbol(data_sym_idx, traveling);

                        self.equalizer.equalize(
                            &self.sym_in,
                            &data_sc,
                            &mut self.sym_out,
                            (&mut out[o * N_DATA_SC..(o + 1) * N_DATA_SC]).try_into().unwrap(),
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
                            self.state = State::Copy(n_sym, *all_sym, *modulation, n_extra, traveling);
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
