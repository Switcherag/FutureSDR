//! 802.11ah receiver v4 — [`v2`](crate::v2) with the per-frame and per-sample
//! copies removed. Identical DSP, identical PHY parameters (2 MHz RF at
//! 4 MSps, 2× oversampled, `FFT_SIZE` 128): this is purely a data-movement
//! optimisation.
//!
//! Blocks (wired sequentially via message ports named `frame`):
//!
//!     StfDetector → CfoCorrector → StoCorrector →
//!     ChannelEstimator → SigDecoder → DataDemod → Decoder
//!
//! Differences from v2:
//!
//!  * [`StfDetector`] pushes the input slice straight into its sample ring
//!    instead of allocating a `Vec` copy of the whole input buffer on every
//!    `work()` call. `sample_total` still advances one sample at a time, so
//!    the detector sees exactly the v2 cadence.
//!  * The four stages that mutate a frame (`CfoCorrector`, `StoCorrector`,
//!    `ChannelEstimator`, `SigDecoder`) take ownership of the `FrameCtx` out
//!    of the incoming `Pmt` via `PmtAny::take` rather than `downcast_ref` +
//!    `clone`. v2 deep-copied `FrameCtx::samples` — the whole buffered frame —
//!    once per stage; v4 moves the heap buffer by pointer.
//!
//! `DataDemod` is unchanged: it only ever borrowed the context.

pub mod channel_estimator;
pub mod cfo_corrector;
pub mod ctx;
pub mod data_demod;
pub mod helpers;
pub mod sig_decoder;
pub mod stf_detector;
pub mod sto_corrector;

pub use channel_estimator::ChannelEstimator;
pub use cfo_corrector::CfoCorrector;
pub use ctx::FrameCtx;
pub use data_demod::DataDemod;
pub use sig_decoder::SigDecoder;
pub use stf_detector::StfDetector;
pub use sto_corrector::StoCorrector;
