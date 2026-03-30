//! Built-in bridge blocks for cross-flowgraph data transfer.
//!
//! These are created automatically by the controller — users never
//! instantiate them directly or reference them in TOML.

use futuresdr::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_BRIDGE_BYTES: usize = 4096 * 8; // 32 KiB, enough for 4096 Complex32 samples

// ════════════════════════════════════════════════════════════════════
// u8 bridges
// ════════════════════════════════════════════════════════════════════

#[derive(Block)]
pub(crate) struct BridgeSinkU8 {
    #[input]
    input: DefaultCpuReader<u8>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSinkU8 {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self { input: DefaultCpuReader::default(), buf }
    }
}

impl Kernel for BridgeSinkU8 {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let n = i.len();
        if n > 0 {
            let mut buf = self.buf.lock().unwrap();
            for &sample in i.iter() {
                if buf.len() >= MAX_BRIDGE_BYTES {
                    buf.pop_front();
                }
                buf.push_back(sample);
            }
            drop(buf);
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

#[derive(Block)]
pub(crate) struct BridgeSourceU8 {
    #[output]
    output: DefaultCpuWriter<u8>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSourceU8 {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self { output: DefaultCpuWriter::default(), buf }
    }
}

impl Kernel for BridgeSourceU8 {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let o = self.output.slice();
        if o.is_empty() {
            return Ok(());
        }

        let mut buf = self.buf.lock().unwrap();
        let available = buf.len().min(o.len());
        if available > 0 {
            for (dst, src) in o.iter_mut().zip(buf.drain(..available)) {
                *dst = src;
            }
            drop(buf);
            self.output.produce(available);
        } else {
            drop(buf);
            io.block_on(async {
                futuresdr::async_io::Timer::after(std::time::Duration::from_millis(50)).await;
            });
        }
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════
// Complex32 bridges  (8 bytes/sample, serialised to VecDeque<u8>)
// ════════════════════════════════════════════════════════════════════

#[derive(Block)]
pub(crate) struct BridgeSinkC32 {
    #[input]
    input: DefaultCpuReader<Complex32>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSinkC32 {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self { input: DefaultCpuReader::default(), buf }
    }
}

impl Kernel for BridgeSinkC32 {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let n = i.len();
        if n > 0 {
            let bytes = unsafe {
                std::slice::from_raw_parts(i.as_ptr() as *const u8, n * 8)
            };
            let mut buf = self.buf.lock().unwrap();
            for &b in bytes {
                if buf.len() >= MAX_BRIDGE_BYTES {
                    buf.pop_front();
                }
                buf.push_back(b);
            }
            drop(buf);
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

#[derive(Block)]
pub(crate) struct BridgeSourceC32 {
    #[output]
    output: DefaultCpuWriter<Complex32>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSourceC32 {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self { output: DefaultCpuWriter::default(), buf }
    }
}

impl Kernel for BridgeSourceC32 {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let o = self.output.slice();
        if o.is_empty() {
            return Ok(());
        }

        let mut buf = self.buf.lock().unwrap();
        let available_samples = buf.len() / 8;
        let to_produce = available_samples.min(o.len());

        if to_produce > 0 {
            let out_bytes = unsafe {
                std::slice::from_raw_parts_mut(o.as_mut_ptr() as *mut u8, to_produce * 8)
            };
            for (dst, src) in out_bytes.iter_mut().zip(buf.drain(..to_produce * 8)) {
                *dst = src;
            }
            drop(buf);
            self.output.produce(to_produce);
        } else {
            drop(buf);
            io.block_on(async {
                futuresdr::async_io::Timer::after(std::time::Duration::from_millis(50)).await;
            });
        }
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════
// f32 bridges  (4 bytes/sample, serialised to VecDeque<u8>)
// ════════════════════════════════════════════════════════════════════

#[derive(Block)]
pub(crate) struct BridgeSinkF32 {
    #[input]
    input: DefaultCpuReader<f32>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSinkF32 {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self { input: DefaultCpuReader::default(), buf }
    }
}

impl Kernel for BridgeSinkF32 {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let n = i.len();
        if n > 0 {
            let bytes = unsafe {
                std::slice::from_raw_parts(i.as_ptr() as *const u8, n * 4)
            };
            let mut buf = self.buf.lock().unwrap();
            for &b in bytes {
                if buf.len() >= MAX_BRIDGE_BYTES {
                    buf.pop_front();
                }
                buf.push_back(b);
            }
            drop(buf);
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

#[derive(Block)]
pub(crate) struct BridgeSourceF32 {
    #[output]
    output: DefaultCpuWriter<f32>,
    buf: Arc<Mutex<VecDeque<u8>>>,
}

impl BridgeSourceF32 {
    pub fn new(buf: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self { output: DefaultCpuWriter::default(), buf }
    }
}

impl Kernel for BridgeSourceF32 {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let o = self.output.slice();
        if o.is_empty() {
            return Ok(());
        }

        let mut buf = self.buf.lock().unwrap();
        let available_samples = buf.len() / 4;
        let to_produce = available_samples.min(o.len());

        if to_produce > 0 {
            let out_bytes = unsafe {
                std::slice::from_raw_parts_mut(o.as_mut_ptr() as *mut u8, to_produce * 4)
            };
            for (dst, src) in out_bytes.iter_mut().zip(buf.drain(..to_produce * 4)) {
                *dst = src;
            }
            drop(buf);
            self.output.produce(to_produce);
        } else {
            drop(buf);
            io.block_on(async {
                futuresdr::async_io::Timer::after(std::time::Duration::from_millis(50)).await;
            });
        }
        Ok(())
    }
}
