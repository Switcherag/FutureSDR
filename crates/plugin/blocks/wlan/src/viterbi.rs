//! Viterbi decoder of the 802.11 convolutional code (K = 7), from
//! `examples/wlan`, taking the code rate and lengths as parameters.

use crate::phy::CodeRate;

const TRACEBACK_MAX: usize = 24;

pub struct ViterbiDecoder {
    rate: CodeRate,
    n_traceback: usize,
    store_pos: usize,

    metric0: [u8; 64],
    metric1: [u8; 64],
    path0: [u8; 64],
    path1: [u8; 64],

    branchtab27: [[u8; 32]; 2],

    mmresult: [u8; 64],
    ppresult: [[u8; 64]; TRACEBACK_MAX],

    depunctured: Vec<u8>,
}

impl ViterbiDecoder {
    /// A decoder for up to `max_coded_bits` coded bits.
    pub fn new(max_coded_bits: usize) -> Self {
        ViterbiDecoder {
            rate: CodeRate::R1_2,
            n_traceback: 0,
            store_pos: 0,

            metric0: [0; 64],
            metric1: [0; 64],
            path0: [0; 64],
            path1: [0; 64],

            branchtab27: [[0; 32]; 2],

            mmresult: [0; 64],
            ppresult: [[0; 64]; TRACEBACK_MAX],

            depunctured: vec![0; 2 * max_coded_bits + 16 * TRACEBACK_MAX],
        }
    }

    fn reset(&mut self, rate: CodeRate) {
        self.rate = rate;

        self.metric0.fill(0);
        self.metric1.fill(0);
        self.path0.fill(0);
        self.path1.fill(0);

        let polys: [usize; 2] = [0x6d, 0x4f];
        for i in 0..32 {
            self.branchtab27[0][i] = u8::from(PARTAB[(2 * i) & polys[0]] > 0);
            self.branchtab27[1][i] = u8::from(PARTAB[(2 * i) & polys[1]] > 0);
        }
        // info!("branchtab27 0: {:?}", self.branchtab27[0]);
        // info!("branchtab27 1: {:?}", self.branchtab27[1]);

        self.store_pos = 0;
        self.mmresult.fill(0);
        self.ppresult.fill([0; 64]);

        self.n_traceback = rate.traceback();
    }

    /// Undo the puncturing of `n_symbols` symbols of `n_cbps` coded bits;
    /// punctured positions become erasures (2). The traceback reads past
    /// the end, where zeros follow, as they did in `examples/wlan` for the
    /// first frame (later frames read the stale bits of earlier ones there).
    /// Zeros rather than erasures: the metric update cannot take a pair of
    /// two erasures.
    fn depuncture(&mut self, in_bits: &[u8], n_symbols: usize, n_cbps: usize) {
        let pattern = self.rate.puncturing();
        let erased = |count: usize| pattern[count % pattern.len()] == 0;
        let mut count = 0;
        for &bit in &in_bits[..n_symbols * n_cbps] {
            while erased(count) {
                self.depunctured[count] = 2;
                count += 1;
            }
            self.depunctured[count] = bit;
            count += 1;
        }
        while erased(count) {
            self.depunctured[count] = 2;
            count += 1;
        }
        self.depunctured[count..].fill(0);
    }

    fn viterbi_butterfly2_generic(&mut self, symbols: &[u8; 4]) {
        let mut metric0 = &mut self.metric0;
        let mut path0 = &mut self.path0;
        let mut metric1 = &mut self.metric1;
        let mut path1 = &mut self.path1;

        // info!("symbols: {:?}", symbols);

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

        // for (j = 0; j < 16; j++) {
        //     sym0v[j] = symbols[0];
        //     sym1v[j] = symbols[1];
        // }
        sym0v[0..16].fill(symbols[0]);
        sym1v[0..16].fill(symbols[1]);

        // for (i = 0; i < 2; i++) {
        for i in 0..2 {
            // if (symbols[0] == 2) {
            //     for (j = 0; j < 16; j++) {
            //         metsvm[j] = d_branchtab27_generic[1].c[(i * 16) + j] ^ sym1v[j];
            //         metsv[j] = 1 - metsvm[j];
            //     }
            // } else if (symbols[1] == 2) {
            //     for (j = 0; j < 16; j++) {
            //         metsvm[j] = d_branchtab27_generic[0].c[(i * 16) + j] ^ sym0v[j];
            //         metsv[j] = 1 - metsvm[j];
            //     }
            // } else {
            //     for (j = 0; j < 16; j++) {
            //         metsvm[j] = (d_branchtab27_generic[0].c[(i * 16) + j] ^ sym0v[j]) +
            //                     (d_branchtab27_generic[1].c[(i * 16) + j] ^ sym1v[j]);
            //         metsv[j] = 2 - metsvm[j];
            //     }
            // }
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
            // for (j = 0; j < 16; j++) {
            //     m0[j] = metric0[(i * 16) + j] + metsv[j];
            //     m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
            //     m2[j] = metric0[(i * 16) + j] + metsvm[j];
            //     m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            // }
            for j in 0..16 {
                m0[j] = metric0[(i * 16) + j] + metsv[j];
                m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
                m2[j] = metric0[(i * 16) + j] + metsvm[j];
                m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            }

            // for (j = 0; j < 16; j++) {
            //     decision0[j] = ((m0[j] - m1[j]) > 0) ? 0xff : 0x0;
            //     decision1[j] = ((m2[j] - m3[j]) > 0) ? 0xff : 0x0;
            //     survivor0[j] = (decision0[j] & m0[j]) | ((~decision0[j]) & m1[j]);
            //     survivor1[j] = (decision1[j] & m2[j]) | ((~decision1[j]) & m3[j]);
            // }
            for j in 0..16 {
                decision0[j] = if m0[j] > m1[j] { 0xff } else { 0x0 };
                decision1[j] = if m2[j] > m3[j] { 0xff } else { 0x0 };
                survivor0[j] = (decision0[j] & m0[j]) | ((!decision0[j]) & m1[j]);
                survivor1[j] = (decision1[j] & m2[j]) | ((!decision1[j]) & m3[j]);
            }
            // for (j = 0; j < 16; j += 2) {
            //     simd_epi16 = path0[(i * 16) + j];
            //     simd_epi16 |= path0[(i * 16) + (j + 1)] << 8;
            //     simd_epi16 <<= 1;
            //     shift0[j] = simd_epi16;
            //     shift0[j + 1] = simd_epi16 >> 8;

            //     simd_epi16 = path0[((i + 2) * 16) + j];
            //     simd_epi16 |= path0[((i + 2) * 16) + (j + 1)] << 8;
            //     simd_epi16 <<= 1;
            //     shift1[j] = simd_epi16;
            //     shift1[j + 1] = simd_epi16 >> 8;
            // }
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

            // for (j = 0; j < 16; j++) {
            //     shift1[j] = shift1[j] + 1;
            // }
            for j in 0..16 {
                shift1[j] += 1;
            }
            // for (j = 0, k = 0; j < 16; j += 2, k++) {
            //     metric1[(2 * i * 16) + j] = survivor0[k];
            //     metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                metric1[(2 * i * 16) + j] = survivor0[k];
                metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            }

            // for (j = 0; j < 16; j++) {
            //     tmp0[j] = (decision0[j] & shift0[j]) | ((~decision0[j]) & shift1[j]);
            // }
            for j in 0..16 {
                tmp0[j] = (decision0[j] & shift0[j]) | ((!decision0[j]) & shift1[j]);
            }
            // for (j = 0, k = 8; j < 16; j += 2, k++) {
            //     metric1[((2 * i + 1) * 16) + j] = survivor0[k];
            //     metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                metric1[((2 * i + 1) * 16) + j] = survivor0[k];
                metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            }
            // for (j = 0; j < 16; j++) {
            //     tmp1[j] = (decision1[j] & shift0[j]) | ((~decision1[j]) & shift1[j]);
            // }
            for j in 0..16 {
                tmp1[j] = (decision1[j] & shift0[j]) | ((!decision1[j]) & shift1[j]);
            }

            // for (j = 0, k = 0; j < 16; j += 2, k++) {
            //     path1[(2 * i * 16) + j] = tmp0[k];
            //     path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                path1[(2 * i * 16) + j] = tmp0[k];
                path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            }
            // for (j = 0, k = 8; j < 16; j += 2, k++) {
            //     path1[((2 * i + 1) * 16) + j] = tmp0[k];
            //     path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                path1[((2 * i + 1) * 16) + j] = tmp0[k];
                path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            }
        }

        metric0 = &mut self.metric1;
        path0 = &mut self.path1;
        metric1 = &mut self.metric0;
        path1 = &mut self.path0;

        // for (j = 0; j < 16; j++) {
        //     sym0v[j] = symbols[2];
        //     sym1v[j] = symbols[3];
        // }
        sym0v[0..16].fill(symbols[2]);
        sym1v[0..16].fill(symbols[3]);

        // for (i = 0; i < 2; i++) {
        for i in 0..2 {
            // if (symbols[2] == 2) {
            //     for (j = 0; j < 16; j++) {
            //         metsvm[j] = d_branchtab27_generic[1].c[(i * 16) + j] ^ sym1v[j];
            //         metsv[j] = 1 - metsvm[j];
            //     }
            // } else if (symbols[3] == 2) {
            //     for (j = 0; j < 16; j++) {
            //         metsvm[j] = d_branchtab27_generic[0].c[(i * 16) + j] ^ sym0v[j];
            //         metsv[j] = 1 - metsvm[j];
            //     }
            // } else {
            //     for (j = 0; j < 16; j++) {
            //         metsvm[j] = (d_branchtab27_generic[0].c[(i * 16) + j] ^ sym0v[j]) +
            //                     (d_branchtab27_generic[1].c[(i * 16) + j] ^ sym1v[j]);
            //         metsv[j] = 2 - metsvm[j];
            //     }
            // }
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
            // for (j = 0; j < 16; j++) {
            //     m0[j] = metric0[(i * 16) + j] + metsv[j];
            //     m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
            //     m2[j] = metric0[(i * 16) + j] + metsvm[j];
            //     m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            // }
            for j in 0..16 {
                m0[j] = metric0[(i * 16) + j] + metsv[j];
                m1[j] = metric0[((i + 2) * 16) + j] + metsvm[j];
                m2[j] = metric0[(i * 16) + j] + metsvm[j];
                m3[j] = metric0[((i + 2) * 16) + j] + metsv[j];
            }
            // for (j = 0; j < 16; j++) {
            //     decision0[j] = ((m0[j] - m1[j]) > 0) ? 0xff : 0x0;
            //     decision1[j] = ((m2[j] - m3[j]) > 0) ? 0xff : 0x0;
            //     survivor0[j] = (decision0[j] & m0[j]) | ((~decision0[j]) & m1[j]);
            //     survivor1[j] = (decision1[j] & m2[j]) | ((~decision1[j]) & m3[j]);
            // }
            for j in 0..16 {
                decision0[j] = if m0[j] > m1[j] { 0xff } else { 0x0 };
                decision1[j] = if m2[j] > m3[j] { 0xff } else { 0x0 };
                survivor0[j] = (decision0[j] & m0[j]) | ((!decision0[j]) & m1[j]);
                survivor1[j] = (decision1[j] & m2[j]) | ((!decision1[j]) & m3[j]);
            }
            // for (j = 0; j < 16; j += 2) {
            //     simd_epi16 = path0[(i * 16) + j];
            //     simd_epi16 |= path0[(i * 16) + (j + 1)] << 8;
            //     simd_epi16 <<= 1;
            //     shift0[j] = simd_epi16;
            //     shift0[j + 1] = simd_epi16 >> 8;

            //     simd_epi16 = path0[((i + 2) * 16) + j];
            //     simd_epi16 |= path0[((i + 2) * 16) + (j + 1)] << 8;
            //     simd_epi16 <<= 1;
            //     shift1[j] = simd_epi16;
            //     shift1[j + 1] = simd_epi16 >> 8;
            // }
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
            // for (j = 0; j < 16; j++) {
            //     shift1[j] = shift1[j] + 1;
            // }
            for j in 0..16 {
                shift1[j] += 1;
            }
            // for (j = 0, k = 0; j < 16; j += 2, k++) {
            //     metric1[(2 * i * 16) + j] = survivor0[k];
            //     metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                metric1[(2 * i * 16) + j] = survivor0[k];
                metric1[(2 * i * 16) + (j + 1)] = survivor1[k];
            }
            // for (j = 0; j < 16; j++) {
            //     tmp0[j] = (decision0[j] & shift0[j]) | ((~decision0[j]) & shift1[j]);
            // }
            for j in 0..16 {
                tmp0[j] = (decision0[j] & shift0[j]) | ((!decision0[j]) & shift1[j]);
            }
            // for (j = 0, k = 8; j < 16; j += 2, k++) {
            //     metric1[((2 * i + 1) * 16) + j] = survivor0[k];
            //     metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(8..) {
                metric1[((2 * i + 1) * 16) + j] = survivor0[k];
                metric1[((2 * i + 1) * 16) + (j + 1)] = survivor1[k];
            }
            // for (j = 0; j < 16; j++) {
            //     tmp1[j] = (decision1[j] & shift0[j]) | ((~decision1[j]) & shift1[j]);
            // }
            for j in 0..16 {
                tmp1[j] = (decision1[j] & shift0[j]) | ((!decision1[j]) & shift1[j]);
            }
            // for (j = 0, k = 0; j < 16; j += 2, k++) {
            //     path1[(2 * i * 16) + j] = tmp0[k];
            //     path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            // }
            for (j, k) in (0..16).step_by(2).zip(0..) {
                path1[(2 * i * 16) + j] = tmp0[k];
                path1[(2 * i * 16) + (j + 1)] = tmp1[k];
            }
            // for (j = 0, k = 8; j < 16; j += 2, k++) {
            //     path1[((2 * i + 1) * 16) + j] = tmp0[k];
            //     path1[((2 * i + 1) * 16) + (j + 1)] = tmp1[k];
            // }
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

        // info!("get output mm0 {:?}", mm0);
        // info!("get output pp0 {:?}", pp0);
        // info!("get output store pos {:?}", self.store_pos);

        for i in 0..4 {
            for j in 0..16 {
                self.mmresult[(i * 16) + j] = mm0[(i * 16) + j];
                self.ppresult[self.store_pos][(i * 16) + j] = pp0[(i * 16) + j];
            }
        }

        // Find out the best final state
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
            // Obtain the state from the output bits
            // by clocking in the output bits in reverse order.
            // The state has only 6 bits
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

    /// Decode `n_data_bits` bits into `out_bits` from `n_symbols` symbols of
    /// `n_cbps` coded bits each, coded at `rate`.
    pub fn decode(
        &mut self,
        rate: CodeRate,
        n_symbols: usize,
        n_cbps: usize,
        n_data_bits: usize,
        in_bits: &[u8],
        out_bits: &mut [u8],
    ) {
        self.reset(rate);
        self.depuncture(in_bits, n_symbols, n_cbps);

        let mut in_count = 0;
        let mut out_count = 0;
        let mut n_decoded = 0;

        while n_decoded < n_data_bits {
            if (in_count % 4) == 0 {
                let index = in_count & !0b11;
                self.viterbi_butterfly2_generic(
                    &self.depunctured[index..index + 4].try_into().unwrap(),
                );

                if (in_count > 0) && (in_count % 16) == 8 {
                    // 8 or 11
                    let c = self.viterbi_get_output_generic();
                    // info!("c: {}", c);

                    if out_count >= self.n_traceback {
                        // info!("c used: {}", c);
                        for i in 0..8 {
                            out_bits[(out_count - self.n_traceback) * 8 + i] = (c >> (7 - i)) & 0x1;
                            n_decoded += 1;
                        }
                    }
                    out_count += 1;
                }
            }
            in_count += 1;
        }
        // info!("decoded bits {}", n_decoded);
    }
}

impl ViterbiDecoder {
    /// Decode like [`decode`](Self::decode), but without Viterbi: the code's
    /// feedforward inverse, `b[n] = A[n-2] + A[n-4] + B[n] + B[n-1] + B[n-2] +
    /// B[n-3] + B[n-4]` (mod 2), since `(D^2 + D^4) gA + (1 + D + D^2 + D^3 +
    /// D^4) gB = 1` for gA = 133 and gB = 171 (octal). It corrects nothing:
    /// a wrong coded bit spoils up to seven decoded ones, and a punctured
    /// (erased) bit counts as 0, so only rate 1/2 decodes whole.
    pub fn decode_hard(
        &mut self,
        rate: CodeRate,
        n_symbols: usize,
        n_cbps: usize,
        n_data_bits: usize,
        in_bits: &[u8],
        out_bits: &mut [u8],
    ) {
        self.reset(rate);
        self.depuncture(in_bits, n_symbols, n_cbps);
        let bit = |k: usize| self.depunctured.get(k).map_or(0, |&b| b & (b != 2) as u8);
        let a = |n: isize| if n < 0 { 0 } else { bit(2 * n as usize) };
        let b = |n: isize| if n < 0 { 0 } else { bit(2 * n as usize + 1) };
        for n in 0..n_data_bits as isize {
            out_bits[n as usize] =
                a(n - 2) ^ a(n - 4) ^ b(n) ^ b(n - 1) ^ b(n - 2) ^ b(n - 3) ^ b(n - 4);
        }
    }
}

/* Parity lookup table */
const PARTAB: [u8; 256] = [
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1,
    1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0,
];
