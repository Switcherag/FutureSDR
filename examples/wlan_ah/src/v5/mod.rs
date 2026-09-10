//! v5 — streaming 802.11ah receiver.
//!
//! Same estimators as [`v2`](crate::v2)/[`v4`](crate::v4), restructured so
//! nothing waits on a whole PPDU. One block replaces the six-stage message
//! pipeline:
//!
//!     StreamingRx (stream Complex32 in, u8 stream out) → Decoder
//!
//! v2/v4 buffer a worst-case frame (76,448 samples ≈ 19 ms at 4 MSps) before
//! processing starts. v5 buffers 1,088 samples (≈0.27 ms) — the preamble —
//! decodes SIG from it, then demodulates each data symbol as it lands.

pub mod dsp;
pub mod helpers;
pub mod streaming_rx;

pub use dsp::PREAMBLE_LEN;
pub use streaming_rx::StreamingRx;
