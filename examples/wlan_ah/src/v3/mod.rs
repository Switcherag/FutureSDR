//! v3 — line-by-line port of the analysis notebook's `decode_ppdu` function.
//!
//! Single all-in-one block that:
//!  * accumulates samples from the source until EOF
//!  * runs the notebook's Schmidl-Cox block-argmax + local-max + 30 dB detector
//!  * for each detection, runs `decode_ppdu` (CFO → STO → h_est → SIG → data
//!    symbols → Viterbi → descrambler → MPDU/PSDU extraction with FCS check)
//!  * emits each successful frame on a `rx_frames` message port
//!
//! Goal: produce the *same* decoded packets as the notebook for the same
//! input file. This is intentionally not streaming-friendly — it favours
//! correctness over throughput so we can isolate algorithmic vs flowgraph
//! issues.

mod ppdu_processor;
pub use ppdu_processor::PpduProcessor;
