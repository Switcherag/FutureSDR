//! File-backed ACK burst: replays a precomputed IQ waveform into a TX radio's
//! input buffer when a `trigger` message arrives.
//!
//! Each waveform is loaded once from a `.cf32` file (interleaved LE f32 I/Q,
//! produced by the `gen_waveforms` side binary). On `trigger(i)` the block
//! pushes `waveforms[i]` into the shared input buffer of a sink
//! [`RadioController`](plugin_host::RadioController) — whose
//! `BridgeSourceC32 → SeifySink` chain then transmits it. Transmitting an ACK
//! is therefore a single `extend()` of a `VecDeque`, with no live PHY on the
//! latency path; a "PHY swap" is just retuning that radio and triggering a
//! different waveform index.
//!
//! # Messages
//! `trigger`: `Pmt::Usize` / `U32` / `U64` — index of the waveform to transmit.

use std::path::Path;

use futuresdr::prelude::*;
use plugin_host::RadioOutputBuf;

#[derive(Block)]
#[message_inputs(trigger)]
#[null_kernel]
pub struct AckBurst {
    waveforms: Vec<Vec<Complex32>>,
    tx_buf: RadioOutputBuf,
}

impl AckBurst {
    /// Load each `.cf32` file in `paths` (in order) and bind the block to the
    /// sink radio's input buffer `tx_buf`. `trigger(i)` transmits `paths[i]`.
    pub fn from_files<P: AsRef<Path>>(paths: &[P], tx_buf: RadioOutputBuf) -> anyhow::Result<Self> {
        let mut waveforms = Vec::with_capacity(paths.len());
        for p in paths {
            waveforms.push(load_cf32(p.as_ref())?);
        }
        Ok(Self { waveforms, tx_buf })
    }

    /// Sample count of each loaded waveform (same order as `from_files`).
    /// The caller uses these to size the post-trigger dwell:
    /// `burst = len / sample_rate`.
    pub fn waveform_lens(&self) -> Vec<usize> {
        self.waveforms.iter().map(|w| w.len()).collect()
    }

    async fn trigger(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let idx = match p {
            Pmt::Usize(v) => v,
            Pmt::U32(v) => v as usize,
            Pmt::U64(v) => v as usize,
            Pmt::Finished => {
                io.finished = true;
                return Ok(Pmt::Ok);
            }
            other => {
                warn!("AckBurst: trigger expects an integer index, got {other:?}");
                return Ok(Pmt::Ok);
            }
        };

        match self.waveforms.get(idx) {
            Some(wave) => {
                // Append the whole burst; the sink radio drains it at the
                // device sample rate and goes idle once the deque empties.
                let mut buf = self.tx_buf.lock().unwrap();
                buf.extend(wave.iter().copied());
            }
            None => warn!(
                "AckBurst: waveform index {idx} out of range (have {})",
                self.waveforms.len()
            ),
        }
        Ok(Pmt::Ok)
    }
}

/// Read interleaved little-endian f32 I/Q (GNU Radio cf32) into `Vec<Complex32>`.
fn load_cf32(path: &Path) -> anyhow::Result<Vec<Complex32>> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    if bytes.len() % 8 != 0 {
        anyhow::bail!(
            "{}: length {} is not a multiple of 8 (interleaved f32 I/Q)",
            path.display(),
            bytes.len()
        );
    }
    let mut wave = Vec::with_capacity(bytes.len() / 8);
    for chunk in bytes.chunks_exact(8) {
        let re = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let im = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        wave.push(Complex32::new(re, im));
    }
    Ok(wave)
}
