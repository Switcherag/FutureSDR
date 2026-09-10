//! v6 — 802.11ah receiver forked from `examples/wlan`'s 11a blocks.
//!
//! A second streaming lineage, independent of [`v5`](crate::v5). Where v5
//! streams v2's Schmidl & Cox / polyfit estimators, v6 takes 11a's blocks
//! as they stand and moves them to the S1G standard:
//!
//!     SyncShort → SyncLong → Fft(128) → FrameEqualizer → Decoder
//!
//! Kept from 11a: the STF threshold-and-plateau detector, the LTF
//! matched-filter correlator with its two-peak CFO estimate, the
//! Sync1/Sync2/Signal/Copy equaliser state machine, and the single
//! common-phase pilot correction.
//!
//! Changed for S1G: 64-point FFT → 128, 48 data subcarriers → 52, the
//! one-symbol SIGNAL field with a parity bit → two-symbol S1G-SIG-A with a
//! CRC-4, and S1G pilot signs with optional traveling pilots.

pub mod frame_equalizer;
pub mod helpers;
pub mod sync_long;
pub mod sync_short;

pub use frame_equalizer::FrameEqualizer;
pub use sync_long::SyncLong;
pub use sync_short::{STF_CORR_WIN, STF_DELAY, STF_POWER_WIN, SyncShort};
