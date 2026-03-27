#![allow(clippy::needless_range_loop)]

use futuresdr::prelude::*;
use std::collections::VecDeque;

// ============================================================
// Constants
// ============================================================

const MAX_PAYLOAD_SIZE: usize = 1500;
const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28;
const MAX_ENCODED_BITS: usize = (16 + 8 * MAX_PSDU_SIZE + 6) * 2 + 288;
const MAX_FRAMES: usize = 1000;

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

    pub fn n_cbps(&self) -> usize {
        match self {
            Mcs::Bpsk_1_2 | Mcs::Bpsk_3_4 => 48,
            Mcs::Qpsk_1_2 | Mcs::Qpsk_3_4 => 96,
            Mcs::Qam16_1_2 | Mcs::Qam16_3_4 => 192,
            Mcs::Qam64_2_3 | Mcs::Qam64_3_4 => 288,
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

    pub fn parse(s: &str) -> Result<Mcs, String> {
        let mut m = s.to_string().replace(['-', '_'], "");
        m.make_ascii_lowercase();
        match m.as_str() {
            "bpsk12" => Ok(Mcs::Bpsk_1_2),
            "bpsk34" => Ok(Mcs::Bpsk_3_4),
            "qpsk12" => Ok(Mcs::Qpsk_1_2),
            "qpsk34" => Ok(Mcs::Qpsk_3_4),
            "qam1612" => Ok(Mcs::Qam16_1_2),
            "qam1634" => Ok(Mcs::Qam16_3_4),
            "qam6423" => Ok(Mcs::Qam64_2_3),
            "qam6434" => Ok(Mcs::Qam64_3_4),
            _ => Err(format!("Invalid MCS {s}")),
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
    pub fn n_data_bits(&self) -> usize { self.n_data_bits }
    pub fn n_pad(&self) -> usize { self.n_pad }
    pub fn n_symbols(&self) -> usize { self.n_symbols }
}

// ============================================================
// Enc (encoding internals)
// ============================================================

struct Enc {
    scrambler_seed: u8,
    bits: [u8; MAX_ENCODED_BITS],
    scrambled: [u8; MAX_ENCODED_BITS],
    encoded: [u8; 2 * MAX_ENCODED_BITS],
    punctured: [u8; 2 * MAX_ENCODED_BITS],
    interleaved: [u8; 2 * MAX_ENCODED_BITS],
    symbols: [u8; 2 * MAX_ENCODED_BITS],
}

impl Enc {
    fn generate_bits(&mut self, data: &[u8]) {
        for i in 0..data.len() {
            for b in 0..8 {
                self.bits[16 + i * 8 + b] = u8::from((data[i] & (1 << b)) > 0);
            }
        }
    }

    fn scramble(&mut self, n_data_bits: usize, n_pad: usize) {
        let mut state = self.scrambler_seed;
        self.scrambler_seed += 1;
        if self.scrambler_seed > 127 {
            self.scrambler_seed = 1;
        }

        let mut feedback;
        for i in 0..n_data_bits {
            feedback = u8::from((state & 64) > 0) ^ u8::from((state & 8) > 0);
            self.scrambled[i] = feedback ^ self.bits[i];
            state = ((state << 1) & 0x7e) | feedback;
        }

        let offset = n_data_bits - n_pad - 6;
        self.scrambled[offset..offset + 6].fill(0);
    }

    fn convolutional_encode(&mut self, n_data_bits: usize) {
        let mut state = 0;
        for i in 0..n_data_bits {
            state = ((state << 1) & 0x7e) | self.scrambled[i];
            self.encoded[i * 2] = (state & 0o155).count_ones() as u8 % 2;
            self.encoded[i * 2 + 1] = (state & 0o117).count_ones() as u8 % 2;
        }
    }

    fn puncture(&mut self, n_data_bits: usize, mcs: Mcs) {
        if matches!(mcs, Mcs::Bpsk_1_2 | Mcs::Qpsk_1_2 | Mcs::Qam16_1_2) {
            self.punctured[0..n_data_bits * 2].copy_from_slice(&self.encoded[0..n_data_bits * 2]);
            return;
        }

        let mut out = 0;
        for i in 0..2 * n_data_bits {
            match mcs {
                Mcs::Qam64_2_3 => {
                    if i % 4 != 3 {
                        self.punctured[out] = self.encoded[i];
                        out += 1;
                    }
                }
                Mcs::Bpsk_3_4 | Mcs::Qpsk_3_4 | Mcs::Qam16_3_4 | Mcs::Qam64_3_4 => {
                    let m = i % 6;
                    if !(m == 3 || m == 4) {
                        self.punctured[out] = self.encoded[i];
                        out += 1;
                    }
                }
                _ => panic!("half-rate case should be handled separately"),
            }
        }
    }

    fn interleave(&mut self, n_cbps: usize, n_bpsc: usize, n_sym: usize) {
        let mut first = vec![0; n_cbps];
        let mut second = vec![0; n_cbps];
        let s = std::cmp::max(n_bpsc / 2, 1);

        for j in 0..n_cbps {
            first[j] = s * (j / s) + ((j + (16 * j / n_cbps)) % s);
        }

        for i in 0..n_cbps {
            second[i] = 16 * i - (n_cbps - 1) * (16 * i / n_cbps);
        }

        for i in 0..n_sym {
            for k in 0..n_cbps {
                self.interleaved[i * n_cbps + k] = self.punctured[i * n_cbps + second[first[k]]];
            }
        }
    }

    fn split_symbols(&mut self, n_bpsc: usize, n_sym: usize) {
        let symbols = n_sym * 48;
        for i in 0..symbols {
            self.symbols[i] = 0;
            for k in 0..n_bpsc {
                self.symbols[i] |= self.interleaved[i * n_bpsc + k] << k;
            }
        }
    }

    fn encode(&mut self, data: &[u8], frame: &FrameParam) {
        self.generate_bits(data);
        self.scramble(frame.n_data_bits(), frame.n_pad());
        self.convolutional_encode(frame.n_data_bits());
        self.puncture(frame.n_data_bits(), frame.mcs());
        self.interleave(
            frame.mcs.n_cbps(),
            frame.mcs.modulation().n_bpsc(),
            frame.n_symbols(),
        );
        self.split_symbols(frame.mcs.modulation().n_bpsc(), frame.n_symbols());
    }
}

// ============================================================
// Encoder Block
// ============================================================

#[derive(Block)]
#[message_inputs(tx)]
pub struct Encoder<O = DefaultCpuWriter<u8>>
where
    O: CpuBufferWriter<Item = u8>,
{
    #[output]
    output: O,
    tx_frames: VecDeque<(Vec<u8>, Mcs)>,
    default_mcs: Mcs,
    current_len: usize,
    current_index: usize,
    enc: Box<Enc>,
}

impl<O> Encoder<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    pub fn new(default_mcs: Mcs) -> Self {
        Self {
            output: O::default(),
            tx_frames: VecDeque::new(),
            default_mcs,
            current_len: 0,
            current_index: 0,
            enc: Box::new(Enc {
                scrambler_seed: 1,
                bits: [0; MAX_ENCODED_BITS],
                scrambled: [0; MAX_ENCODED_BITS],
                encoded: [0; 2 * MAX_ENCODED_BITS],
                punctured: [0; 2 * MAX_ENCODED_BITS],
                interleaved: [0; 2 * MAX_ENCODED_BITS],
                symbols: [0; 2 * MAX_ENCODED_BITS],
            }),
        }
    }

    async fn tx(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Blob(data) => {
                if self.tx_frames.len() >= MAX_FRAMES {
                    warn!(
                        "WLAN Encoder: max number of frames already in TX queue ({}). Dropping.",
                        MAX_FRAMES
                    );
                } else if data.len() > MAX_PSDU_SIZE {
                    warn!(
                        "WLAN Encoder: TX frame too large ({}, max {}). Dropping.",
                        data.len(),
                        MAX_PSDU_SIZE
                    );
                } else {
                    self.tx_frames.push_back((data, self.default_mcs));
                }
            }
            Pmt::Any(a) => {
                if let Some((data, mcs)) = a.downcast_ref::<(Vec<u8>, Option<Mcs>)>() {
                    let data = data.clone();
                    if self.tx_frames.len() >= MAX_FRAMES {
                        warn!(
                            "WLAN Encoder: max number of frames already in TX queue ({}). Dropping.",
                            MAX_FRAMES
                        );
                    } else if data.len() > MAX_PSDU_SIZE {
                        warn!(
                            "WLAN Encoder: TX frame too large ({}, max {}). Dropping.",
                            data.len(),
                            MAX_PSDU_SIZE
                        );
                    } else if let Some(m) = mcs {
                        self.tx_frames.push_back((data, *m));
                    } else {
                        self.tx_frames.push_back((data, self.default_mcs));
                    }
                }
            }
            Pmt::Finished => {
                io.finished = true;
            }
            x => {
                warn!(
                    "WLAN Encoder: received wrong PMT type in TX callback. {:?}",
                    x
                );
            }
        }
        Ok(Pmt::Null)
    }
}

impl<O> Kernel for Encoder<O>
where
    O: CpuBufferWriter<Item = u8>,
{
    async fn work(
        &mut self,
        _io: &mut WorkIo,
        _m: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        loop {
            let (out, mut out_tags) = self.output.slice_with_tags();
            if out.is_empty() {
                break;
            }

            if self.current_len == 0 {
                if let Some((data, mcs)) = self.tx_frames.pop_front() {
                    let frame = FrameParam::new(mcs, data.len());
                    self.enc.encode(&data, &frame);
                    self.current_len = frame.n_symbols() * 48;
                    self.current_index = 0;
                    out_tags.add_tag(0, Tag::NamedAny("wifi_start".to_string(), Box::new(frame)));
                } else {
                    break;
                }
            } else {
                let n = std::cmp::min(out.len(), self.current_len - self.current_index);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.enc.symbols.as_ptr().add(self.current_index),
                        out.as_mut_ptr(),
                        n,
                    );
                }

                self.output.produce(n);
                self.current_index += n;

                if self.current_index == self.current_len {
                    self.current_len = 0;
                }
            }
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "WlanEncoder",
    description: "WLAN 802.11 convolutional encoder with interleaving",
    config: String,
    create: |cfg, _id| {
        let mcs = Mcs::parse(&cfg).expect("invalid MCS string");
        Encoder::<DefaultCpuWriter<u8>>::new(mcs)
    }
}
