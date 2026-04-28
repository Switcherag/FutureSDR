//! 802.11ah receiver v2 — direct port of the Python notebook `decode_ppdu`,
//! split into one FutureSDR block per pipeline stage.
//!
//! Blocks (wired sequentially via message ports named `frame`):
//!
//!     StfDetector → CfoCorrector → StoCorrector →
//!     ChannelEstimator → SigDecoder → DataDemod → Decoder
//!
//! Each block consumes/emits a `Pmt::Any(Box<FrameCtx>)`. The final
//! `DataDemod` converts to the same `u8` stream shape that the existing
//! `Decoder` expects (52 bytes per data symbol, tagged with `FrameParam`).

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
