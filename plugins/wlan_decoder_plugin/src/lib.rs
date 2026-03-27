#![allow(clippy::needless_range_loop)]

use futuresdr::prelude::*;

// ============================================================
// Constants
// ============================================================

pub const MAX_PAYLOAD_SIZE: usize = 1500;
pub const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28; // MAC, CRC
pub const MAX_SYM: usize = ((16 + 8 * MAX_PSDU_SIZE + 6) / 24) + 1;
pub const MAX_ENCODED_BITS: usize = (16 + 8 * MAX_PSDU_SIZE + 6) * 2 + 288;

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
    pub fn depuncture_pattern(&self) -> &'static [usize] {
        match self {
            Mcs::Bpsk_1_2 | Mcs::Qpsk_1_2 | Mcs::Qam16_1_2 => &[1, 1],
            Mcs::Bpsk_3_4 | Mcs::Qpsk_3_4 | Mcs::Qam16_3_4 | Mcs::Qam64_3_4 => {
                &[1, 1, 1, 0, 0, 1]
            }
            Mcs::Qam64_2_3 => &[1, 1, 1, 0],
        }
    }

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
    #[allow(dead_code)]
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
    pub fn psdu_size(&self) -> usize {
        self.psdu_size
    }
    pub fn mcs(&self) -> Mcs {
        self.mcs
    }
    pub fn n_data_bits(&self) -> usize {
        self.n_data_bits
    }
    pub fn n_symbols(&self) -> usize {
        self.n_symbols
    }
}

// ============================================================
// ViterbiDecoder
// ============================================================

const TRACEBACK_MAX: usize = 24;

pub struct ViterbiDecoder {
    frame_param: FrameParam,
    n_traceback: usize,
    store_pos: usize,

    metric0: [u8; 64],
    metric1: [u8; 64],
    path0: [u8; 64],
    path1: [u8; 64],

    branchtab27: [[u8; 32]; 2],

    mmresult: [u8; 64],
    ppresult: [[u8; 64]; TRACEBACK_MAX],

    depunctured: [u8; MAX_ENCODED_BITS],
}

impl ViterbiDecoder {
    pub fn new() -> Self {
        ViterbiDecoder {
            frame_param: FrameParam::new(Mcs::Bpsk_1_2, 0),
            n_traceback: 0,
            store_pos: 0,

            metric0: [0; 64],
            metric1: [0; 64],
            path0: [0; 64],
            path1: [0; 64],

            branchtab27: [[0; 32]; 2],

            mmresult: [0; 64],
            ppresult: [[0; 64]; TRACEBACK_MAX],

            depunctured: [0; MAX_ENCODED_BITS],
        }
    }

    fn reset(&mut self, param: FrameParam) {
        self.frame_param = param;

        self.metric0.fill(0);
        self.metric1.fill(0);
        self.path0.fill(0);
        self.path1.fill(0);

        let polys: [usize; 2] = [0x6d, 0x4f];
        for i in 0..32 {
            self.branchtab27[0][i] = u8::from(PARTAB[(2 * i) & polys[0]] > 0);
            self.branchtab27[1][i] = u8::from(PARTAB[(2 * i) & polys[1]] > 0);
        }

        self.store_pos = 0;
        self.mmresult.fill(0);
        self.ppresult.fill([0; 64]);

        match self.frame_param.mcs() {
            Mcs::Bpsk_1_2 | Mcs::Qpsk_1_2 | Mcs::Qam16_1_2 => {
                self.n_traceback = 5;
            }
            Mcs::Bpsk_3_4 | Mcs::Qpsk_3_4 | Mcs::Qam16_3_4 | Mcs::Qam64_3_4 => {
                self.n_traceback = 10;
            }
            Mcs::Qam64_2_3 => {
                self.n_traceback = 9;
            }
        }
    }

    pub fn depuncture(&mut self, in_bits: &[u8]) {
        if self.n_traceback == 5 {
            self.depunctured[0..in_bits.len()].copy_from_slice(in_bits);
        } else {
            let pattern = self.frame_param.mcs.depuncture_pattern();
            let n_cbps = self.frame_param.mcs().n_cbps();
            let mut count = 0;

            for i in 0..self.frame_param.n_symbols() {
                for k in 0..n_cbps {
                    while pattern[count % pattern.len()] == 0 {
                        self.depunctured[count] = 2;
                        count += 1;
                    }
                    self.depunctured[count] = in_bits[i * n_cbps + k];
                    count += 1;
                    while pattern[count % pattern.len()] == 0 {
                        self.depunctured[count] = 2;
                        count += 1;
                    }
                }
            }
        }
    }

    fn viterbi_butterfly2_generic(&mut self, symbols: &[u8; 4]) {
        let mut metric0 = &mut self.metric0;
        let mut path0 = &mut self.path0;
        let mut metric1 = &mut self.metric1;
        let mut path1 = &mut self.path1;

        let mut m0 = [0u8; 16];
        let mut m1 = [0u8; 16];
        let mut m2 = [0u8; 16];
        let mut m3 = [0u8; 16];
        let mut decision0 = [0u8; 16];
        let mut decision1 = [0u8; 16];
        let mut survivor0 = [0u8; 16];
        let mut survivor1 = [0u8; 16];
        let mut metsv = [0u8; 16];
        let mut metsvm = [0u8; 16];
        let mut shift0 = [0u8; 16];
        let mut shift1 = [0u8; 16];
        let mut tmp0 = [0u8; 16];
        let mut tmp1 = [0u8; 16];
        let mut sym0v = [0u8; 16];
        let mut sym1v = [0u8; 16];
        let mut simd_epi16: u16;

        sym0v[0..16].fill(symbols[0]);
        sym1v[0..16].fill(symbols[1]);

        for i in 0..2 {
            if symbols[0] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[1][(i * 16) + j] ^ sym1v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else if symbols[1] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[0][(i * 16) + j] ^ sym0v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else {
                for j in 0..16 {
                    metsvm[j] = (self.branchtab27[0][(i * 16) + j] ^ sym0v[j])
                        + (self.branchtab27[1][(i * 16) + j] ^ sym1v[j]);
                    metsv[j] = 2 - metsvm[j];
                }
            }
            for j in 0..16 {
                m0[j] = metric0[(i * 16) + j] + metsv[j];
                m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
                m2[j] = metric0[(i * 16) + j] + metsvm[j];
                m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            }
            for j in 0..16 {
                decision0[j] = if m0[j] > m1[j] { 0xff } else { 0x0 };
                decision1[j] = if m2[j] > m3[j] { 0xff } else { 0x0 };
                survivor0[j] = (decision0[j] & m0[j]) | ((!decision0[j]) & m1[j]);
                survivor1[j] = (decision1[j] & m2[j]) | ((!decision1[j]) & m3[j]);
            }
            for j in (0..16).step_by(2) {
                simd_epi16 = path0[(i * 16) + j] as u16;
                simd_epi16 |= (path0[(i * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift0[j] = simd_epi16 as u8;
                shift0[j + 1] = (simd_epi16 >> 8) as u8;

                simd_epi16 = path0[((i + 2) * 16) + j] as u16;
                simd_epi16 |= (path0[((i + 2) * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift1[j] = simd_epi16 as u8;
                shift1[j + 1] = (simd_epi16 >> 8) as u8;
            }
            for j in 0..16 {
                shift1[j] += 1;
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                metric1[(2 * i * 16) + j] = survivor0[k];
                metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp0[j] = (decision0[j] & shift0[j]) | ((!decision0[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                metric1[((2 * i + 1) * 16) + j] = survivor0[k];
                metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp1[j] = (decision1[j] & shift0[j]) | ((!decision1[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                path1[(2 * i * 16) + j] = tmp0[k];
                path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                path1[((2 * i + 1) * 16) + j] = tmp0[k];
                path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            }
        }

        metric0 = &mut self.metric1;
        path0 = &mut self.path1;
        metric1 = &mut self.metric0;
        path1 = &mut self.path0;

        sym0v[0..16].fill(symbols[2]);
        sym1v[0..16].fill(symbols[3]);

        for i in 0..2 {
            if symbols[2] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[1][(i * 16) + j] ^ sym1v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else if symbols[3] == 2 {
                for j in 0..16 {
                    metsvm[j] = self.branchtab27[0][(i * 16) + j] ^ sym0v[j];
                    metsv[j] = 1 - metsvm[j];
                }
            } else {
                for j in 0..16 {
                    metsvm[j] = (self.branchtab27[0][(i * 16) + j] ^ sym0v[j])
                        + (self.branchtab27[1][(i * 16) + j] ^ sym1v[j]);
                    metsv[j] = 2 - metsvm[j];
                }
            }
            for j in 0..16 {
                m0[j] = metric0[(i * 16) + j] + metsv[j];
                m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
                m2[j] = metric0[(i * 16) + j] + metsvm[j];
                m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            }
            for j in 0..16 {
                decision0[j] = if m0[j] > m1[j] { 0xff } else { 0x0 };
                decision1[j] = if m2[j] > m3[j] { 0xff } else { 0x0 };
                survivor0[j] = (decision0[j] & m0[j]) | ((!decision0[j]) & m1[j]);
                survivor1[j] = (decision1[j] & m2[j]) | ((!decision1[j]) & m3[j]);
            }
            for j in (0..16).step_by(2) {
                simd_epi16 = path0[(i * 16) + j] as u16;
                simd_epi16 |= (path0[(i * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift0[j] = simd_epi16 as u8;
                shift0[j + 1] = (simd_epi16 >> 8) as u8;

                simd_epi16 = path0[((i + 2) * 16) + j] as u16;
                simd_epi16 |= (path0[((i + 2) * 16) + (j + 1)] as u16) << 8;
                simd_epi16 <<= 1;
                shift1[j] = simd_epi16 as u8;
                shift1[j + 1] = (simd_epi16 >> 8) as u8;
            }
            for j in 0..16 {
                shift1[j] += 1;
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                metric1[(2 * i * 16) + j] = survivor0[k];
                metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp0[j] = (decision0[j] & shift0[j]) | ((!decision0[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                metric1[((2 * i + 1) * 16) + j] = survivor0[k];
                metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            }
            for j in 0..16 {
                tmp1[j] = (decision1[j] & shift0[j]) | ((!decision1[j]) & shift1[j]);
            }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                path1[(2 * i * 16) + j] = tmp0[k];
                path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                path1[((2 * i + 1) * 16) + j] = tmp0[k];
                path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            }
        }
    }

    fn viterbi_get_output_generic(&mut self) -> u8 {
        let mm0 = &mut self.metric0;
        let pp0 = &mut self.path0;

        self.store_pos = (self.store_pos + 1) % self.n_traceback;

        for i in 0..4 {
            for j in 0..16 {
                self.mmresult[(i * 16) + j] = mm0[(i * 16) + j];
                self.ppresult[self.store_pos][(i * 16) + j] = pp0[(i * 16) + j];
            }
        }

        let mut beststate = 0;
        let mut bestmetric = self.mmresult[beststate];
        let mut minmetric = self.mmresult[beststate];

        for i in 1..64 {
            if self.mmresult[i] > bestmetric {
                bestmetric = self.mmresult[i];
                beststate = i;
            }
            if self.mmresult[i] < minmetric {
                minmetric = self.mmresult[i];
            }
        }

        let mut pos = self.store_pos;
        for _ in 0..(self.n_traceback - 1) {
            beststate = (self.ppresult[pos][beststate] >> 2) as usize;
            pos = (pos + self.n_traceback - 1) % self.n_traceback;
        }

        for i in 0..4 {
            for j in 0..16 {
                pp0[(i * 16) + j] = 0;
                mm0[(i * 16) + j] -= minmetric;
            }
        }

        self.ppresult[pos][beststate]
    }

    pub fn decode(&mut self, frame: FrameParam, in_bits: &[u8], out_bits: &mut [u8]) {
        self.reset(frame);
        self.depuncture(in_bits);

        let mut in_count = 0;
        let mut out_count = 0;
        let mut n_decoded = 0;

        while n_decoded < self.frame_param.n_data_bits() {
            if (in_count % 4) == 0 {
                let index = in_count & !0b11;
                self.viterbi_butterfly2_generic(
                    &self.depunctured[index..index + 4].try_into().unwrap(),
                );

                if (in_count > 0) && (in_count % 16) == 8 {
                    let c = self.viterbi_get_output_generic();
                    if out_count >= self.n_traceback {
                        for i in 0..8 {
                            out_bits[(out_count - self.n_traceback) * 8 + i] =
                                (c >> (7 - i)) & 0x1;
                            n_decoded += 1;
                        }
                    }
                    out_count += 1;
                }
            }
            in_count += 1;
        }
    }
}

impl Default for ViterbiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

const PARTAB: [u8; 256] = [
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0,
    0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1,
    0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0,
    0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0,
    0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1,
    0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0,
    0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
];

// ============================================================
// WLAN Decoder Block
// ============================================================

#[derive(Block)]
#[message_outputs(rx_frames, rftap)]
pub struct Decoder<I = DefaultCpuReader<u8>>
where
    I: CpuBufferReader<Item = u8>,
{
    #[input]
    input: I,
    frame_complete: bool,
    frame_param: FrameParam,
    decoder: ViterbiDecoder,
    copied: usize,
    rx_symbols: [u8; 48 * MAX_SYM],
    rx_bits: [u8; MAX_ENCODED_BITS],
    deinterleaved_bits: [u8; MAX_ENCODED_BITS],
    decoded_bits: [u8; MAX_ENCODED_BITS],
    out_bytes: [u8; MAX_PSDU_SIZE + 2],
}

impl<I> Decoder<I>
where
    I: CpuBufferReader<Item = u8>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            frame_complete: true,
            frame_param: FrameParam::new(Mcs::Bpsk_1_2, 0),
            decoder: ViterbiDecoder::new(),
            copied: 0,
            rx_symbols: [0; 48 * MAX_SYM],
            rx_bits: [0; MAX_ENCODED_BITS],
            deinterleaved_bits: [0; MAX_ENCODED_BITS],
            decoded_bits: [0; MAX_ENCODED_BITS],
            out_bytes: [0; MAX_PSDU_SIZE + 2],
        }
    }

    fn deinterleave(&mut self) {
        let n_cbps = self.frame_param.mcs().n_cbps();
        let n_bpsc = self.frame_param.mcs().modulation().n_bpsc();
        let mut first = vec![0usize; n_cbps];
        let mut second = vec![0usize; n_cbps];
        let s = std::cmp::max(n_bpsc / 2, 1);

        for j in 0..n_cbps {
            first[j] = s * (j / s) + ((j + (16 * j / n_cbps)) % s);
        }
        for i in 0..n_cbps {
            second[i] = 16 * i - (n_cbps - 1) * (16 * i / n_cbps);
        }

        for i in 0..self.frame_param.n_symbols() {
            for k in 0..n_cbps {
                self.deinterleaved_bits[i * n_cbps + second[first[k]]] =
                    self.rx_bits[i * n_cbps + k];
            }
        }
    }

    fn decode(&mut self) -> bool {
        let syms = self.frame_param.n_symbols();
        let bpsc = self.frame_param.mcs().modulation().n_bpsc();
        for i in 0..syms * 48 {
            for k in 0..bpsc {
                self.rx_bits[i * bpsc + k] = u8::from((self.rx_symbols[i] & (1 << k)) > 0);
            }
        }

        self.deinterleave();
        self.decoder.decode(
            self.frame_param.clone(),
            &self.deinterleaved_bits,
            &mut self.decoded_bits,
        );
        self.descramble();

        let crc = crc32fast::hash(&self.out_bytes[2..self.frame_param.psdu_size() + 2]);
        crc == 558161692
    }

    fn descramble(&mut self) {
        let decoded_bits = &self.decoded_bits;

        let mut state = 0;
        self.out_bytes[0..self.frame_param.psdu_size() + 2].fill(0);

        for i in 0..7 {
            if decoded_bits[i] > 0 {
                state |= 1 << (6 - i);
            }
        }

        self.out_bytes[0] = state;

        let mut feedback;
        let mut bit;

        for i in 7..self.frame_param.psdu_size() * 8 + 16 {
            feedback = u8::from((state & 64) > 0) ^ u8::from((state & 8) > 0);
            bit = feedback ^ (decoded_bits[i] & 1);
            self.out_bytes[i / 8] |= bit << (i % 8);
            state = ((state << 1) & 0x7e) | feedback;
        }
    }
}

impl<I> Default for Decoder<I>
where
    I: CpuBufferReader<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I> Kernel for Decoder<I>
where
    I: CpuBufferReader<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let (mut input, in_tags) = self.input.slice_with_tags();

        if let Some((index, any)) = in_tags.iter().find_map(|x| match x {
            ItemTag {
                index,
                tag: Tag::NamedAny(n, any),
            } => {
                if n == "wifi_start" {
                    Some((index, any))
                } else {
                    None
                }
            }
            _ => None,
        }) {
            if *index == 0 {
                if !self.frame_complete {
                    warn!("decoder: previous frame not complete, canceling.");
                }
                let frame_param = any.downcast_ref::<FrameParam>().unwrap();
                if frame_param.n_symbols() <= MAX_SYM && frame_param.psdu_size() <= MAX_PSDU_SIZE {
                    self.frame_param = frame_param.clone();
                    self.copied = 0;
                    self.frame_complete = false;
                } else {
                    warn!("decoder: frame too large, dropping. ({:?})", frame_param);
                }
            } else {
                input = &input[0..*index];
            }
        }

        let max_i = input.len() / 48;
        let mut i = 0;

        while i < max_i {
            if self.copied < self.frame_param.n_symbols() {
                self.rx_symbols[(self.copied * 48)..((self.copied + 1) * 48)]
                    .copy_from_slice(&input[(i * 48)..((i + 1) * 48)]);
            }

            i += 1;
            self.copied += 1;

            if self.copied == self.frame_param.n_symbols() {
                self.frame_complete = true;

                if self.decode() {
                    let mut blob = vec![0; self.frame_param.psdu_size() - 4];
                    blob.copy_from_slice(&self.out_bytes[2..self.frame_param.psdu_size() - 2]);

                    let mut rftap = vec![0; blob.len() + 12];
                    rftap[0..4].copy_from_slice("RFta".as_bytes());
                    rftap[4..6].copy_from_slice(&3u16.to_le_bytes());
                    rftap[6..8].copy_from_slice(&1u16.to_le_bytes());
                    rftap[8..12].copy_from_slice(&105u32.to_le_bytes());
                    rftap[12..].copy_from_slice(&blob);
                    mio.post("rx_frames", Pmt::Blob(blob)).await?;
                    mio.post("rftap", Pmt::Blob(rftap)).await?;
                }

                i = max_i;
                break;
            }
        }

        self.input.consume(i * 48);
        if self.input.finished() && i == max_i {
            mio.post("rx_frames", Pmt::Finished).await?;
            io.finished = true;
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "Decoder",
    description: "WLAN Viterbi decoder with CRC check",
    config: (),
    create: |_cfg, _id| {
        Decoder::<DefaultCpuReader<u8>>::new()
    }
}
