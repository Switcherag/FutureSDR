//! Built-in bridge blocks for cross-flowgraph data transfer.
//!
//! These are created automatically by the controller — users never
//! instantiate them directly or reference them in TOML.
//!
//! Each bridge pair (Sink + Source) uses a typed `VecDeque<T>` so that
//! data is stored at item granularity. The sink does a single bulk
//! `extend()` per `work()` call; the source does a single `copy_from_slice()`
//! via `make_contiguous()`. No byte-by-byte loops.

use futuresdr::futures::SinkExt;
use futuresdr::futures::channel::mpsc;
use futuresdr::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// ── NamedMessagePipe ─────────────────────────────────────────────────
// Forwards received PMTs to a shared mpsc channel, tagged with a name.
// Used by FlowgraphController's `[[controller_taps]]` feature.

#[derive(Block)]
#[message_inputs(r#in)]
#[null_kernel]
pub(crate) struct NamedMessagePipe {
    name: String,
    sender: mpsc::Sender<(String, Pmt)>,
}

impl NamedMessagePipe {
    pub fn new(name: String, sender: mpsc::Sender<(String, Pmt)>) -> Self {
        Self { name, sender }
    }

    async fn r#in(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let _ = self.sender.send((self.name.clone(), p)).await;
        Ok(Pmt::Null)
    }
}

// Item-count capacity for each type.
const CAP_U8:  usize =   32_768; // 32 KiB × 1 byte
const CAP_F32: usize =    8_192; // 32 KiB / 4 bytes
const CAP_C32: usize = 4_194_304; // 32 MiB / 8 bytes, gives more headroom for swap-time stalls

macro_rules! make_bridge {
    ($sink:ident, $source:ident, $t:ty, $cap:expr) => {
        // ── Sink ──────────────────────────────────────────────────────
        #[derive(Block)]
        pub(crate) struct $sink {
            #[input]
            input: DefaultCpuReader<$t>,
            buf: Arc<Mutex<VecDeque<$t>>>,
            dropped_total: u64,
        }

        impl $sink {
            pub fn new(buf: Arc<Mutex<VecDeque<$t>>>) -> Self {
                Self { input: DefaultCpuReader::default(), buf, dropped_total: 0 }
            }
        }

        impl Kernel for $sink {
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
                    // Drop whole items to make room, never partial items.
                    let overflow = (buf.len() + n).saturating_sub($cap);
                    if overflow > 0 {
                        buf.drain(..overflow);
                        let prev = self.dropped_total;
                        self.dropped_total += overflow as u64;
                        // Log on first overflow and then every 1M dropped samples.
                        if prev == 0 || self.dropped_total / 1_000_000 > prev / 1_000_000 {
                            eprintln!(
                                "bridge {}: dropped {} items ({} total)",
                                stringify!($sink), overflow, self.dropped_total
                            );
                        }
                    }
                    buf.extend(i.iter().copied());
                    drop(buf);
                    self.input.consume(n);
                }
                if self.input.finished() {
                    io.finished = true;
                }
                Ok(())
            }
        }

        // ── Source ────────────────────────────────────────────────────
        #[derive(Block)]
        pub(crate) struct $source {
            #[output]
            output: DefaultCpuWriter<$t>,
            buf: Arc<Mutex<VecDeque<$t>>>,
        }

        impl $source {
            pub fn new(buf: Arc<Mutex<VecDeque<$t>>>) -> Self {
                Self { output: DefaultCpuWriter::default(), buf }
            }
        }

        impl Kernel for $source {
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
                let to_produce = buf.len().min(o.len());
                if to_produce > 0 {
                    // Use `as_slices()` instead of `make_contiguous()`: the
                    // latter rotates the entire deque to a single slice — an
                    // O(N) memmove where N = buf.len(), held under the lock.
                    // With a deep buffer (post-swap pile-up, up to 32 MiB for
                    // C32) that rotation alone can take ms, blocking the Sink
                    // long enough for the upstream SeifySource to overflow.
                    // as_slices() returns the two contiguous halves in place,
                    // so we copy only `to_produce` items in (at most) two
                    // chunks — lock hold time scales with the copy, not the
                    // total buffered size.
                    let (first, second) = buf.as_slices();
                    let n1 = first.len().min(to_produce);
                    o[..n1].copy_from_slice(&first[..n1]);
                    if n1 < to_produce {
                        let n2 = to_produce - n1;
                        o[n1..to_produce].copy_from_slice(&second[..n2]);
                    }
                    buf.drain(..to_produce);
                    drop(buf);
                    self.output.produce(to_produce);
                } else {
                    drop(buf);
                    // Short sleep to yield the executor without busy-waiting.
                    // 50µs keeps latency low while avoiding a spin loop.
                    io.block_on(async {
                        futuresdr::async_io::Timer::after(
                            std::time::Duration::from_micros(50),
                        )
                        .await;
                    });
                }
                Ok(())
            }
        }
    };
}

make_bridge!(BridgeSinkU8,  BridgeSourceU8,  u8,       CAP_U8);
make_bridge!(BridgeSinkF32, BridgeSourceF32, f32,      CAP_F32);
make_bridge!(BridgeSinkC32, BridgeSourceC32, Complex32, CAP_C32);
