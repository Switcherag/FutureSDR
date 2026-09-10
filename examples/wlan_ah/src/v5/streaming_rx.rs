//! Streaming 802.11ah receiver: v2's estimators, symbol-at-a-time.
//!
//! v2/v4 buffer a worst-case PPDU (76,448 samples ≈ 19 ms at 4 MSps) before
//! any stage looks at a frame. Every estimator they run, though, only reads
//! the preamble — CFO and STO from the STF, `h_est` from the LTF, `FrameParam`
//! from SIG — and the data path's sole inter-symbol coupling is the pilot
//! phase carry. So this block buffers `PREAMBLE_LEN` (1088 samples ≈ 0.27 ms),
//! decodes SIG there, and then demodulates each data symbol as it arrives.

use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use std::collections::VecDeque;

use crate::v5::dsp::{
    PREAMBLE_LEN, PilotTracker, decode_sig, estimate_cfo, estimate_channel, estimate_sto,
    sig_symbols,
};
use crate::v5::helpers::{TCP, TS};
use crate::{FFT_SIZE, FrameParam, N_DATA_SC, ViterbiDecoder};

const LAG: usize = FFT_SIZE / 4;
const BOXCAR_LEN: usize = 2 * TS - LAG;
const DETECT_WIN: usize = 6 * TS;
const LOCAL_MAX_R: usize = 2 * TS;
const NORM_WIN_LEN: usize = TS / 2;
const NEED: usize = DETECT_WIN + 2 * LOCAL_MAX_R;
const DETECT_THRESHOLD_DB: f32 = 30.0;

/// Samples that must exist past a detection candidate before the preamble can
/// be processed: the preamble itself plus a symbol of slack so a negative
/// integer STO can still be read out of the ring.
const DETECT_LOOKAHEAD: usize = PREAMBLE_LEN + TS;

enum State {
    /// Running the Schmidl & Cox correlator, no frame in hand.
    Search,
    /// Detection at `start`; waiting for `DETECT_LOOKAHEAD` samples past it.
    AwaitPreamble { start: usize },
    /// SIG decoded; walking the data symbols one at a time.
    Data(Box<DataState>),
}

struct DataState {
    /// Absolute index the CFO ramp is referenced to (the raw detection point).
    cfo_ref: usize,
    cfo: f32,
    sto_frac: f32,
    h_est: Vec<Complex32>,
    param: FrameParam,
    tracker: PilotTracker,
    /// Absolute index of the next symbol's start (including its guard).
    cursor: usize,
    /// Index within the `extra + n_symbols` walk.
    j: usize,
    extra: usize,
    n_sym: usize,
    tagged: bool,
}

#[derive(Block)]
#[message_outputs(symbols, sync_info)]
pub struct StreamingRx<I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<u8>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    #[input]
    input: I,
    #[output]
    output: O,

    ring: VecDeque<Complex32>,
    ring_base: usize,
    processed: usize,

    ac_sum: Complex32,
    ac_ring: VecDeque<Complex32>,
    corr_ring: VecDeque<f32>,
    corr_base: usize,

    state: State,
    /// Detections found by the correlator but not yet decoded.
    ///
    /// The correlator runs over *every* sample, independent of what the
    /// decoder is doing. Advancing it only while searching (the obvious
    /// single-block design) silently drops any frame that arrives while the
    /// previous one is still being demodulated — which is precisely the
    /// short-IFS case.
    pending: VecDeque<usize>,
    /// Absolute index one past the last sample consumed by a decoded frame,
    /// used to discard detections that land inside it.
    frame_end: usize,
    decoder: ViterbiDecoder,
    sig_scratch: [u8; 48],
    /// Demodulated symbols waiting for room in the output stream. Staging
    /// here keeps the state machine free of the output borrow; 52 bytes a
    /// symbol, and it only grows when the downstream is backed up.
    out_queue: VecDeque<(Option<FrameParam>, [u8; N_DATA_SC])>,
    syms_out: Vec<Complex32>,
    debug_print: bool,
}

impl<I, O> StreamingRx<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    pub fn new() -> Self {
        Self::new_with_debug_print(false)
    }

    pub fn new_with_debug_print(debug_print: bool) -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            ring: VecDeque::with_capacity(4 * DETECT_LOOKAHEAD),
            ring_base: 0,
            processed: 0,
            ac_sum: Complex32::new(0.0, 0.0),
            ac_ring: VecDeque::with_capacity(BOXCAR_LEN),
            corr_ring: VecDeque::with_capacity(NEED + DETECT_WIN),
            corr_base: 0,
            state: State::Search,
            pending: VecDeque::new(),
            frame_end: 0,
            decoder: ViterbiDecoder::new(),
            sig_scratch: [0; 48],
            out_queue: VecDeque::new(),
            syms_out: Vec::new(),
            debug_print,
        }
    }

    fn pushed(&self) -> usize {
        self.ring_base + self.ring.len()
    }

    fn have(&self, start: usize, len: usize) -> bool {
        start >= self.ring_base && start + len <= self.pushed()
    }

    /// Copy `out.len()` samples starting at absolute index `start`, applying
    /// the CFO de-rotation referenced to `cfo_ref`.
    fn copy_cfo(&self, start: usize, cfo: f32, cfo_ref: usize, out: &mut [Complex32]) -> bool {
        if !self.have(start, out.len()) {
            return false;
        }
        for (i, o) in out.iter_mut().enumerate() {
            let abs = start + i;
            let n = abs as f32 - cfo_ref as f32;
            *o = self.ring[abs - self.ring_base] * Complex32::from_polar(1.0, -cfo * n);
        }
        true
    }

    fn copy_raw(&self, start: usize, out: &mut [Complex32]) -> bool {
        if !self.have(start, out.len()) {
            return false;
        }
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.ring[start + i - self.ring_base];
        }
        true
    }

    fn push_corr(&mut self, x: Complex32, x_delayed: Complex32) {
        let prod = x * x_delayed.conj();
        self.ac_sum += prod;
        self.ac_ring.push_back(prod);
        if self.ac_ring.len() > BOXCAR_LEN {
            self.ac_sum -= self.ac_ring.pop_front().unwrap();
        }
        if self.ac_ring.len() == BOXCAR_LEN {
            let corr_idx = self.processed - 1 - LAG;
            if self.corr_ring.is_empty() {
                self.corr_base = corr_idx;
            }
            self.corr_ring.push_back(self.ac_sum.norm());
        }
    }

    fn avg_power(&self, start: usize, end: usize) -> Option<f32> {
        if start < self.ring_base || end > self.pushed() || start >= end {
            return None;
        }
        let mut sum = 0.0f32;
        for abs in start..end {
            sum += self.ring[abs - self.ring_base].norm_sqr();
        }
        Some(sum / (end - start) as f32)
    }

    /// Run the correlator over every sample pushed since the last call,
    /// queueing each detection. Never gated on decoder state.
    fn advance_detector(&mut self) {
        while self.processed < self.pushed() {
            let x = self.ring[self.processed - self.ring_base];
            self.processed += 1;
            if self.processed > LAG {
                let delayed_abs = self.processed - 1 - LAG;
                if delayed_abs < self.ring_base {
                    continue;
                }
                let delayed = self.ring[delayed_abs - self.ring_base];
                self.push_corr(x, delayed);
            }

            while self.corr_ring.len() >= NEED {
                let mut max_val = f32::NEG_INFINITY;
                let mut max_rel = 0usize;
                for i in 0..DETECT_WIN {
                    let v = self.corr_ring[LOCAL_MAX_R + i];
                    if v > max_val {
                        max_val = v;
                        max_rel = i;
                    }
                }
                let arg_buf = LOCAL_MAX_R + max_rel;
                let abs_corr_idx = self.corr_base + arg_buf;
                let start_idx = abs_corr_idx.saturating_sub(BOXCAR_LEN - 1);

                let mut local_max = f32::NEG_INFINITY;
                for i in (arg_buf - LOCAL_MAX_R)..(arg_buf + LOCAL_MAX_R) {
                    if self.corr_ring[i] > local_max {
                        local_max = self.corr_ring[i];
                    }
                }
                let is_local_max = max_val >= local_max;

                let norm = if start_idx >= TS {
                    self.avg_power(start_idx - TS, start_idx - TS + NORM_WIN_LEN)
                } else {
                    None
                };
                let max_value_norm = match norm {
                    Some(v) if v > 0.0 => max_val / v,
                    _ => f32::NAN,
                };

                self.corr_ring.drain(0..DETECT_WIN);
                self.corr_base += DETECT_WIN;

                if is_local_max
                    && max_value_norm.is_finite()
                    && max_value_norm >= 10f32.powf(DETECT_THRESHOLD_DB / 10.0)
                {
                    self.pending.push_back(start_idx);
                }
            }
        }
    }

    /// Run CFO → STO → channel → SIG over the buffered preamble.
    fn process_preamble(&mut self, start: usize) -> Option<DataState> {
        // CFO from the raw STF.
        let mut stf = [Complex32::new(0.0, 0.0); 2 * TS];
        if !self.copy_raw(start, &mut stf) {
            return None;
        }
        let cfo = estimate_cfo(&stf);

        // STO from the CFO-corrected STF.
        let mut sto_buf = vec![Complex32::new(0.0, 0.0); 2 * TS + FFT_SIZE];
        if !self.copy_cfo(start, cfo, start, &mut sto_buf) {
            return None;
        }
        let total_sto = estimate_sto(&sto_buf);

        let sto_int = total_sto.round() as i32;
        let sto_frac = total_sto - sto_int as f32;
        let origin = (start as i64 + sto_int as i64).max(0) as usize;

        // Preamble from the STO-corrected origin. The CFO ramp stays
        // referenced to `start`, matching v2's order (CFO applied first, then
        // the buffer front trimmed by the integer STO).
        let mut pre = vec![Complex32::new(0.0, 0.0); PREAMBLE_LEN];
        if !self.copy_cfo(origin, cfo, start, &mut pre) {
            return None;
        }

        let h_est = estimate_channel(&pre, sto_frac)?;
        let sig_eq = sig_symbols(&pre, &h_est, sto_frac)?;
        let param = decode_sig(&mut self.decoder, &mut self.sig_scratch, &sig_eq)?;

        if self.debug_print {
            info!(
                "[v5] SIG ok: mcs={:?} n_sym={} psdu={} long={} sgi={} sto={:.2} cfo={:.5}",
                param.mcs(),
                param.n_symbols(),
                param.psdu_size(),
                param.is_long,
                param.short_gi,
                total_sto,
                cfo
            );
        }

        let extra = if param.is_long { 3usize } else { 0 };
        let n_sym = param.n_symbols();
        Some(DataState {
            cfo_ref: start,
            cfo,
            sto_frac,
            h_est,
            param,
            tracker: PilotTracker::new(),
            cursor: origin + 6 * TS,
            j: 0,
            extra,
            n_sym,
            tagged: false,
        })
    }

    /// Drop ring history nothing can still reference.
    fn trim_ring(&mut self) {
        let corr_need = self.processed.saturating_sub(2 * TS + LAG + BOXCAR_LEN);
        let mut keep = match &self.state {
            State::Search => corr_need,
            State::AwaitPreamble { start } => corr_need.min(start.saturating_sub(TS)),
            State::Data(d) => corr_need.min(d.cursor.saturating_sub(TS)),
        };
        if let Some(&oldest) = self.pending.front() {
            keep = keep.min(oldest.saturating_sub(TS));
        }
        while self.ring_base < keep && !self.ring.is_empty() {
            self.ring.pop_front();
            self.ring_base += 1;
        }
    }
}

impl<I, O> Default for StreamingRx<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I, O> Kernel for StreamingRx<I, O>
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
        let input_len = {
            let input = self.input.slice();
            self.ring.extend(input.iter().copied());
            input.len()
        };
        self.input.consume(input_len);

        // Phase 0 — correlate every new sample, whatever the decoder is doing.
        self.advance_detector();

        // Phase 1 — advance the state machine. The state is moved out each
        // iteration so calls like `self.advance_detector()` do not collide
        // with a borrow of `self.state`.
        const MAX_QUEUED: usize = 64;
        loop {
            if self.out_queue.len() >= MAX_QUEUED {
                break;
            }
            match std::mem::replace(&mut self.state, State::Search) {
                State::Search => {
                    // Drop detections that landed inside the frame just decoded.
                    let next = loop {
                        match self.pending.pop_front() {
                            Some(s) if s < self.frame_end => continue,
                            other => break other,
                        }
                    };
                    match next {
                        Some(start) => self.state = State::AwaitPreamble { start },
                        None => break,
                    }
                }
                State::AwaitPreamble { start } => {
                    if self.pushed() < start + DETECT_LOOKAHEAD {
                        self.state = State::AwaitPreamble { start };
                        break;
                    }
                    self.state = match self.process_preamble(start) {
                        Some(d) => State::Data(Box::new(d)),
                        None => State::Search,
                    };
                }
                State::Data(mut d) => {
                    if d.j >= d.extra + d.n_sym {
                        self.frame_end = d.cursor;
                        if !self.syms_out.is_empty() {
                            mio.post("symbols", Pmt::VecCF32(std::mem::take(&mut self.syms_out)))
                                .await?;
                        }
                        continue; // state already reset to Search
                    }

                    let sgi = d.param.short_gi && d.j >= d.extra + 1;
                    let gi = if sgi { TCP / 2 } else { TCP };
                    let win_start = d.cursor + gi / 2;

                    if !self.have(win_start, FFT_SIZE) {
                        self.state = State::Data(d);
                        break;
                    }
                    // The leading S1G_LONG symbols are FFT'd and then never
                    // read by v2, so just step over them.
                    if d.j < d.extra {
                        d.cursor += FFT_SIZE + gi;
                        d.j += 1;
                        self.state = State::Data(d);
                        continue;
                    }

                    let mut blk = [Complex32::new(0.0, 0.0); FFT_SIZE];
                    for (i, b) in blk.iter_mut().enumerate() {
                        let abs = win_start + i;
                        let n = abs as f32 - d.cfo_ref as f32;
                        *b = self.ring[abs - self.ring_base]
                            * Complex32::from_polar(1.0, -d.cfo * n);
                    }

                    let nsym = d.j - d.extra;
                    let mut bytes = [0u8; N_DATA_SC];
                    d.tracker.demod_symbol(
                        &blk,
                        &d.h_est,
                        d.sto_frac,
                        nsym,
                        sgi,
                        d.param.traveling_pilots,
                        d.param.mcs().modulation(),
                        &mut bytes,
                        &mut self.syms_out,
                    );

                    let tag = if d.tagged {
                        None
                    } else {
                        d.tagged = true;
                        Some(d.param.clone())
                    };
                    self.out_queue.push_back((tag, bytes));

                    d.cursor += FFT_SIZE + gi;
                    d.j += 1;
                    self.state = State::Data(d);
                }
            }
        }

        // Phase 2 — drain staged symbols into the output stream.
        let (out, mut out_tags) = self.output.slice_with_tags();
        let mut o = 0usize;
        while !self.out_queue.is_empty() && out.len() - o >= N_DATA_SC {
            let (tag, bytes) = self.out_queue.pop_front().unwrap();
            if let Some(param) = tag {
                out_tags.add_tag(o, Tag::NamedAny("wifi_start".to_string(), Box::new(param)));
            }
            out[o..o + N_DATA_SC].copy_from_slice(&bytes);
            o += N_DATA_SC;
        }

        self.output.produce(o);
        self.trim_ring();

        if !self.out_queue.is_empty() {
            io.call_again = true;
        }
        if self.input.finished() && self.processed >= self.pushed() && self.out_queue.is_empty() {
            io.finished = true;
        }
        Ok(())
    }
}
