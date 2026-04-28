//! Streaming sync_long that follows the Python notebook's `decode_ppdu`
//! technique: STF-only autocorrelation for CFO, STF spectrum FFT for STO.
//! LTF is used downstream for channel estimation only.
//!
//! Pipeline:
//!   1. wifi_start tag from sync_short → estimate STF start via local
//!      sliding-window autocorrelation argmax (with a small pre-buffer so we
//!      can search backward in time as well as forward).
//!   2. Coarse CFO from Tcp-lag autocorrelation; apply.
//!   3. Fine CFO from Ts-lag autocorrelation; apply.
//!   4. Coarse STO via padded DFT of the per-STF-subcarrier response.
//!   5. Fine STO via linear phase slope across STF subcarriers.
//!   6. Round STO to integer, shift the start index, output 2×Tu LTF samples
//!      (post-CFO-correction). Continue State::Copy on subsequent symbols.

use std::collections::VecDeque;

use futuresdr::prelude::*;

use crate::v2::helpers::{TCP, TS, TU, dft_shift, frac_shift_ramp, linear_slope, stf_freq};
use crate::{CP_LEN, FFT_SIZE, SYMBOL_LEN, sc};

/// Pre-buffer length retained while in Search state. Lets us look backward
/// from wifi_start when sync_short tagged late inside the STF.
const PRE_BUF: usize = 2 * SYMBOL_LEN; // 160
/// Post-tag samples needed for STF analysis + LTF output.
/// Worst case: STF_start could be PRE_BUF before tag, then need 2·Ts STF +
/// 2·Tu LTF + small margin for STO.
const POST_REQ: usize = 4 * SYMBOL_LEN; // 320

#[derive(Debug)]
enum State {
    Search,
    Sync(f32),
    Copy(usize, f32),
}

#[derive(Block)]
#[message_outputs(corr_mag, sync_info, ltf_td)]
pub struct SyncLongV2<I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    input: I,
    #[output]
    output: O,
    state: State,
    pre_buf: VecDeque<Complex32>,
}

impl<I, O> SyncLongV2<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            state: State::Search,
            pre_buf: VecDeque::with_capacity(PRE_BUF + 1),
        }
    }
}

impl<I, O> Default for SyncLongV2<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Sliding boxcar Tu/4-lag autocorrelation `|Σ_{i=0..win-1} x[k+i+lag]·x*[k+i]|²`.
/// Argmax k = STF start (when both x[k..] and x[k+lag..] lie inside the STF).
fn find_stf_start(work: &[Complex32]) -> Option<usize> {
    let lag = TU / 4;
    let win = 2 * TS - lag;
    if work.len() < win + lag {
        return None;
    }
    let n = work.len() - win - lag + 1;
    let mut sum = Complex32::new(0.0, 0.0);
    for i in 0..win {
        sum += work[i + lag] * work[i].conj();
    }
    let mut best_pow = sum.norm_sqr();
    let mut best_k = 0usize;
    for k in 1..n {
        sum -= work[k - 1 + lag] * work[k - 1].conj();
        sum += work[k + win - 1 + lag] * work[k + win - 1].conj();
        let p = sum.norm_sqr();
        if p > best_pow {
            best_pow = p;
            best_k = k;
        }
    }
    Some(best_k)
}

/// Coarse + fine CFO from the two-symbol STF starting at `stf_start`.
/// Returns the total CFO in rad/sample (positive = positive Doppler).
fn estimate_cfo(work: &[Complex32], stf_start: usize) -> Option<f32> {
    if stf_start + 2 * TS > work.len() {
        return None;
    }
    let y = &work[stf_start..stf_start + 2 * TS];

    // Coarse: Tcp-lag autocorrelation over the full 2·Ts window.
    let mut c = Complex32::new(0.0, 0.0);
    for i in TCP..(2 * TS) {
        c += y[i] * y[i - TCP].conj();
    }
    let coarse = c.arg() / (TCP as f32);

    // Apply coarse to a working copy of the STF.
    let mut yc = vec![Complex32::new(0.0, 0.0); 2 * TS];
    for (n, &s) in y.iter().enumerate() {
        yc[n] = s * Complex32::from_polar(1.0, -coarse * (n as f32));
    }

    // Fine: Ts-lag autocorrelation of corrected STF.
    let mut f = Complex32::new(0.0, 0.0);
    for i in TS..(2 * TS) {
        f += yc[i] * yc[i - TS].conj();
    }
    let fine = f.arg() / (TS as f32);

    Some(coarse + fine)
}

/// Coarse + fine STO from the STF spectrum (mirrors v2/sto_corrector).
/// `work` must have CFO already applied.
fn estimate_sto(work: &[Complex32], stf_start: usize) -> Option<(i32, f32)> {
    let cp_offset = TCP / 2;
    let cp_corr = -(TCP as f32 / 2.0);
    let stf_ref = stf_freq();

    if stf_start + cp_offset + TS + FFT_SIZE > work.len() {
        return None;
    }

    let mut stf_avg = [Complex32::new(0.0, 0.0); FFT_SIZE];
    for j in 0..2 {
        let t0 = stf_start + cp_offset + j * TS;
        let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
        for n in 0..FFT_SIZE {
            blk[n] = work[t0 + n];
        }
        let fdom = dft_shift(&blk);
        let ramp = frac_shift_ramp(cp_corr);
        for k in 0..FFT_SIZE {
            if stf_ref[k].norm_sqr() > 0.0 {
                stf_avg[k] += fdom[k] * ramp[k] * stf_ref[k].conj();
            }
        }
    }

    let mut stf_idxs: Vec<i32> = Vec::with_capacity(12);
    let mut stf_vals: Vec<Complex32> = Vec::with_capacity(12);
    for off in -26i32..=26 {
        let k = sc(off);
        if stf_ref[k].norm_sqr() > 0.0 {
            stf_idxs.push(off);
            stf_vals.push(stf_avg[k]);
        }
    }
    if stf_vals.is_empty() {
        return None;
    }

    // Coarse STO: zero-padded DFT of the compact STF response.
    let n = stf_vals.len() + 1;
    let mut padded = vec![Complex32::new(0.0, 0.0); n];
    let half = stf_vals.len() / 2;
    padded[..half].copy_from_slice(&stf_vals[..half]);
    padded[n - half..].copy_from_slice(&stf_vals[half..]);
    let mut mags = vec![0.0f32; n];
    for kk in 0..n {
        let mut acc = Complex32::new(0.0, 0.0);
        for tt in 0..n {
            let angle = -2.0 * std::f32::consts::PI * (kk as f32) * (tt as f32) / (n as f32);
            acc += padded[tt] * Complex32::from_polar(1.0, angle);
        }
        mags[kk] = acc.norm();
    }
    let mut argmax = 0usize;
    for i in 1..n {
        if mags[i] > mags[argmax] {
            argmax = i;
        }
    }
    let mut idx = argmax as i32;
    if idx as usize >= n / 2 {
        idx -= n as i32;
    }
    let coarse_sto = -(idx as f32) / (4.0 * n as f32) * (TU as f32);

    // Fine STO via polyfit of per-SC phase across STF subcarriers.
    let mut phases = vec![0.0f32; stf_vals.len()];
    let mut avg_vec = Complex32::new(0.0, 0.0);
    for i in 0..stf_vals.len() {
        let rot = Complex32::from_polar(
            1.0,
            2.0 * std::f32::consts::PI * coarse_sto * (stf_idxs[i] as f32) / (TU as f32),
        );
        let v = stf_vals[i] * rot;
        avg_vec += v;
        phases[i] = v.arg();
    }
    let avg_phase = avg_vec.arg();
    let xs: Vec<f32> = stf_idxs.iter().map(|&i| i as f32).collect();
    let ys: Vec<f32> = phases
        .iter()
        .map(|&pp| {
            let mut d = pp - avg_phase;
            while d > std::f32::consts::PI {
                d -= 2.0 * std::f32::consts::PI;
            }
            while d < -std::f32::consts::PI {
                d += 2.0 * std::f32::consts::PI;
            }
            d
        })
        .collect();
    let slope = linear_slope(&xs, &ys);
    let fine_sto = -slope / (2.0 * std::f32::consts::PI) * (TU as f32);
    let total = coarse_sto + fine_sto;
    Some((total.round() as i32, total - total.round()))
}

impl<I, O> Kernel for SyncLongV2<I, O>
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

        // Look for wifi_start tag in the current input slice.
        let tag = in_tags.iter().find_map(|x| match x {
            ItemTag {
                index,
                tag: Tag::NamedF32(n, f),
            } if n == "wifi_start" => Some((*index, *f)),
            _ => None,
        });

        match self.state {
            State::Search => {
                if let Some((idx, f)) = tag {
                    if idx == 0 {
                        // Tag aligned: enter Sync state and re-call.
                        self.state = State::Sync(f);
                        io.call_again = true;
                    } else {
                        // Consume samples up to (not including) tag, pushing
                        // pre-tag samples into pre_buf so we can look backward.
                        for &s in &input[..idx] {
                            self.pre_buf.push_back(s);
                            if self.pre_buf.len() > PRE_BUF {
                                self.pre_buf.pop_front();
                            }
                        }
                        self.input.consume(idx);
                    }
                } else {
                    // No tag: drain all into pre_buf and consume.
                    for &s in input.iter() {
                        self.pre_buf.push_back(s);
                        if self.pre_buf.len() > PRE_BUF {
                            self.pre_buf.pop_front();
                        }
                    }
                    self.input.consume(input_len);
                }
            }
            State::Sync(f_short) => {
                if input_len < POST_REQ {
                    if self.input.finished() {
                        io.finished = true;
                    }
                    return Ok(());
                }
                if out.len() < 2 * FFT_SIZE {
                    return Ok(());
                }

                // Build a working window: pre_buf samples prepended to the
                // current input so STF detection can search backward.
                let pre_len = self.pre_buf.len();
                let mut work: Vec<Complex32> = Vec::with_capacity(pre_len + POST_REQ);
                work.extend(self.pre_buf.iter().copied());
                work.extend_from_slice(&input[..POST_REQ]);

                // Step 1: locate STF start within `work`.
                let stf_start = match find_stf_start(&work) {
                    Some(k) => k,
                    None => return Ok(()),
                };

                // Step 2: estimate CFO from STF.
                let cfo = match estimate_cfo(&work, stf_start) {
                    Some(c) => c,
                    None => return Ok(()),
                };

                // Step 3: apply CFO correction to a working copy.
                let mut work_corr = work.clone();
                for (n, s) in work_corr.iter_mut().enumerate() {
                    *s *= Complex32::from_polar(1.0, -cfo * (n as f32));
                }

                // Step 4: estimate STO from the corrected STF spectrum.
                let (sto_int, sto_frac) =
                    estimate_sto(&work_corr, stf_start).unwrap_or((0, 0.0));

                // Step 5: adjust STF start by integer STO and locate LTF1.
                let adj_stf = (stf_start as i32 + sto_int).max(0) as usize;
                let ltf1_start = adj_stf + 2 * TS;
                if ltf1_start + 2 * FFT_SIZE > work_corr.len() {
                    return Ok(());
                }

                // Diagnostics
                let info = vec![
                    pre_len as f32,
                    stf_start as f32,
                    sto_int as f32,
                    sto_frac,
                    cfo,
                    f_short,
                ];
                println!(
                    "SYNC_LONG_V2: pre_len={} stf_start={} sto_int={} sto_frac={:.3} cfo={:.6} f_short={:.6}",
                    pre_len, stf_start, sto_int, sto_frac, cfo, f_short
                );
                let ltf_td: Vec<Complex32> =
                    work_corr[ltf1_start..ltf1_start + 2 * FFT_SIZE].to_vec();
                mio.post("sync_info", Pmt::VecF32(info)).await?;
                mio.post("ltf_td", Pmt::VecCF32(ltf_td.clone())).await?;

                // Output the two LTF symbols (CFO-corrected).
                for i in 0..(2 * FFT_SIZE) {
                    out[i] = work_corr[ltf1_start + i];
                }
                out_tags.add_tag(
                    0,
                    Tag::NamedF32("wifi_start".to_string(), f_short + cfo),
                );

                // Bookkeeping: how much of the pre_buf and input did we consume?
                let total_used = ltf1_start + 2 * FFT_SIZE;
                let pre_used = std::cmp::min(pre_len, total_used);
                let input_used = total_used - pre_used;
                for _ in 0..pre_used {
                    self.pre_buf.pop_front();
                }
                self.input.consume(input_used);
                self.output.produce(2 * FFT_SIZE);

                self.state = State::Copy(0, cfo);
                io.call_again = true;
            }
            State::Copy(n_copied, freq_offset) => {
                // Optional: re-tag mid-burst → restart.
                if let Some((idx, f)) = tag {
                    if idx == 0 {
                        self.state = State::Sync(f);
                        io.call_again = true;
                        return Ok(());
                    }
                }

                let m = std::cmp::min(input_len, out.len());
                let syms = m / SYMBOL_LEN;
                for i in 0..syms {
                    for k in 0..FFT_SIZE {
                        out[i * FFT_SIZE + k] = input[i * SYMBOL_LEN + CP_LEN + k]
                            * Complex32::from_polar(
                                1.0,
                                -freq_offset
                                    * (n_copied + i * SYMBOL_LEN + 2 * FFT_SIZE + CP_LEN + k)
                                        as f32,
                            );
                    }
                }
                self.input.consume(syms * SYMBOL_LEN);
                self.output.produce(syms * FFT_SIZE);
                self.state = State::Copy(n_copied + syms * SYMBOL_LEN, freq_offset);
            }
        }

        if self.input.finished() && input_len == 0 {
            io.finished = true;
        }

        Ok(())
    }
}
