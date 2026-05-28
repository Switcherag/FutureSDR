//! BladeRF 2.0 (Micro) — **self-contained seify driver scaffold**.
//!
//! Accessed via `driver=bladerf2` in seify args, e.g.:
//!
//! ```ignore
//! let dev = seify::Device::from_args("driver=bladerf2")?;
//! ```
//!
//! ## Status
//!
//! Scaffold. Every operation returns `Error::NotSupported`. This file is
//! the **only** place to extend when implementing the real driver — there
//! is intentionally no separate `libbladerf-rs` crate involved (unlike
//! `impls/bladerf1.rs`, which wraps the upstream `libbladerf-rs`).
//!
//! ## What to implement, in order
//!
//! 1. **USB transport**: probe by VID `0x2cf0` / PID `0x5250` (bladeRF 2
//!    Micro). Claim USB interface, identify bulk RX/TX endpoints.
//!    Reference: `bladeRF/host/libraries/libbladeRF/src/backend/usb/libusb.c`
//!    and the rust-native bladerf1 transport at
//!    `libbladerf-rs/src/transport/usb.rs`.
//! 2. **NIOS protocol bridge**: pack/unpack the on-FPGA NIOS request/
//!    response frames. Reference:
//!    `bladeRF/host/libraries/libbladeRF/src/board/bladerf2/capabilities.c`
//!    plus the bladerf1 NIOS client.
//! 3. **AD9361 SPI control** — the bulk of the work. The bladeRF 2 uses
//!    Analog Devices AD9361 for RX/TX. Implement register-level access
//!    via the NIOS bridge, then PLL setup, gain, sample-rate, bandwidth.
//!    Reference: `bladeRF/host/libraries/libAD936X/`.
//! 4. **Streaming**: bulk DMA for sync RX/TX (SC16_Q11), the meta variant
//!    for timestamps.
//! 5. **Quick tune**: cache AD9361 register state for a frequency and
//!    re-apply on demand. Mirror the bladerf1 `protocol::packet_retune`
//!    pattern. This is the feature you actually want for fast PHY swaps.
//!
//! Each step is independent — wire it through `DeviceTrait` here as
//! it lands.

use crate::{Args, Direction, Driver, Error, Range, RangeItem};
use num_complex::Complex32;

/// USB VID for Nuand BladeRF devices (shared with bladeRF 1).
pub const BLADERF2_USB_VID: u16 = 0x2cf0;
/// USB PID for the BladeRF 2.0 Micro.
pub const BLADERF2_USB_PID: u16 = 0x5250;

/// Sample format on the BladeRF 2 USB interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// Signed 16-bit Q11 IQ.
    Sc16Q11,
    /// Signed 16-bit Q11 IQ with embedded per-buffer timestamp metadata
    /// (required for `schedule_retune`).
    Sc16Q11Meta,
}

/// Gain control mode for the AD9361 RFIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainMode {
    /// Manual gain — driver writes the gain register directly.
    Manual,
    /// AD9361 fast-attack AGC.
    FastAttackAgc,
    /// AD9361 slow-attack AGC.
    SlowAttackAgc,
    /// AD9361 hybrid AGC.
    HybridAgc,
}

/// Per-stage gain on the AD9361 RFIC. Used by `set_gain_element` to
/// address specific blocks (LNA, mixer, etc.) rather than the composite
/// gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainStage {
    /// Composite gain (sum of all internal stages).
    Full,
    /// Front-end LNA.
    Lna,
    /// Mixer gain.
    Mixer,
    /// Baseband amplifier.
    Bba,
    /// TX attenuator.
    TxAtten,
}

impl GainStage {
    /// String key as used by seify's `set_gain_element` / `gain_element`.
    pub const fn as_str(self) -> &'static str {
        match self {
            GainStage::Full => "FULL",
            GainStage::Lna => "LNA",
            GainStage::Mixer => "MIX",
            GainStage::Bba => "BBA",
            GainStage::TxAtten => "ATT",
        }
    }
}

/// Quick-tune snapshot: the AD9361 register state required to re-tune
/// to a previously visited frequency without recalculating PLL params.
///
/// Populate via `BladeRf2::capture_quick_tune(freq_hz)` (TODO), apply via
/// `BladeRf2::apply_quick_tune(&qt)` (TODO). The exact contents depend on
/// the AD9361 — the C struct in `libbladeRF.h` carries `nios_profile`,
/// `rffe_profile`, `port`, `spdt`.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuickTune {
    /// Profile slot number in the NIOS FPGA.
    pub nios_profile: u16,
    /// Profile slot number in the AD9361 RFFE.
    pub rffe_profile: u8,
    /// RFFE port settings.
    pub port: u8,
    /// External SPDT switch position.
    pub spdt: u8,
}

/// BladeRF 2.0 device handle.
///
/// **Scaffold** — holds no USB transport yet. Constructors fail with
/// `Error::NotSupported`.
pub struct BladeRf2 {
    _placeholder: (),
}

impl BladeRf2 {
    /// Enumerate BladeRF 2.0 devices currently connected to the host.
    ///
    /// Returns an empty list until USB enumeration is implemented. When
    /// implemented, each entry should be an args string of the form
    /// `"driver=bladerf2, bus_id=<n>, address=<n>"` — same convention as
    /// the bladerf1 probe so callers can address a specific device.
    pub fn probe(_args: &Args) -> Result<Vec<Args>, Error> {
        Ok(Vec::new())
    }

    /// Open a BladeRF 2.0 by args. Currently always returns `NotSupported`.
    ///
    /// When implemented: recognize `bus_id=`/`address=`, `serial=`, or
    /// neither (= open first available). On Linux, accept `fd=<n>` to
    /// open a pre-claimed USB file descriptor (sandbox-friendly, mirrors
    /// the bladerf1 pattern).
    pub fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let _args: Args = args.try_into().or(Err(Error::ValueError))?;
        Err(Error::NotSupported)
    }

    /// Capture a `QuickTune` snapshot of the current AD9361 state, so
    /// `apply_quick_tune` can later switch back to this freq cheaply.
    /// Currently unimplemented.
    pub fn capture_quick_tune(&self, _freq_hz: f64) -> Result<QuickTune, Error> {
        Err(Error::NotSupported)
    }

    /// Apply a previously captured `QuickTune` — fast retune path,
    /// equivalent to `bladerf_schedule_retune(BLADERF_RETUNE_NOW, &qt)`.
    /// Currently unimplemented.
    pub fn apply_quick_tune(&self, _qt: &QuickTune) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
}

/// RX streamer wrapper. All methods unimplemented.
pub struct RxStreamer {
    _placeholder: (),
}

/// TX streamer wrapper. All methods unimplemented.
pub struct TxStreamer {
    _placeholder: (),
}

impl crate::RxStreamer for RxStreamer {
    fn mtu(&self) -> Result<usize, Error> { Err(Error::NotSupported) }
    fn activate_at(&mut self, _t: Option<i64>) -> Result<(), Error> { Err(Error::NotSupported) }
    fn deactivate_at(&mut self, _t: Option<i64>) -> Result<(), Error> { Err(Error::NotSupported) }
    fn read(&mut self, _bufs: &mut [&mut [Complex32]], _to_us: i64) -> Result<usize, Error> {
        Err(Error::NotSupported)
    }
}

impl crate::TxStreamer for TxStreamer {
    fn mtu(&self) -> Result<usize, Error> { Err(Error::NotSupported) }
    fn activate_at(&mut self, _t: Option<i64>) -> Result<(), Error> { Err(Error::NotSupported) }
    fn deactivate_at(&mut self, _t: Option<i64>) -> Result<(), Error> { Err(Error::NotSupported) }
    fn write(
        &mut self,
        _bufs: &[&[Complex32]],
        _at_ns: Option<i64>,
        _end_burst: bool,
        _to_us: i64,
    ) -> Result<usize, Error> { Err(Error::NotSupported) }
    fn write_all(
        &mut self,
        _bufs: &[&[Complex32]],
        _at_ns: Option<i64>,
        _end_burst: bool,
        _to_us: i64,
    ) -> Result<(), Error> { Err(Error::NotSupported) }
}

fn empty_range() -> Range {
    Range::new(vec![RangeItem::Interval(0.0, 0.0)])
}

impl crate::DeviceTrait for BladeRf2 {
    type RxStreamer = RxStreamer;
    type TxStreamer = TxStreamer;

    fn as_any(&self) -> &dyn std::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn driver(&self) -> Driver { Driver::BladeRf2 }

    fn id(&self) -> Result<String, Error> { Err(Error::NotSupported) }
    fn info(&self) -> Result<Args, Error> { Ok(Args::default()) }
    fn num_channels(&self, _d: Direction) -> Result<usize, Error> { Err(Error::NotSupported) }
    fn full_duplex(&self, _d: Direction, _c: usize) -> Result<bool, Error> { Err(Error::NotSupported) }

    fn rx_streamer(&self, _ch: &[usize], _args: Args) -> Result<RxStreamer, Error> {
        Err(Error::NotSupported)
    }
    fn tx_streamer(&self, _ch: &[usize], _args: Args) -> Result<TxStreamer, Error> {
        Err(Error::NotSupported)
    }

    fn antennas(&self, _d: Direction, _c: usize) -> Result<Vec<String>, Error> { Ok(Vec::new()) }
    fn antenna(&self, _d: Direction, _c: usize) -> Result<String, Error> { Err(Error::NotSupported) }
    fn set_antenna(&self, _d: Direction, _c: usize, _name: &str) -> Result<(), Error> { Err(Error::NotSupported) }

    fn supports_agc(&self, _d: Direction, _c: usize) -> Result<bool, Error> { Ok(false) }
    fn enable_agc(&self, _d: Direction, _c: usize, _agc: bool) -> Result<(), Error> { Err(Error::NotSupported) }
    fn agc(&self, _d: Direction, _c: usize) -> Result<bool, Error> { Err(Error::NotSupported) }

    fn gain_elements(&self, _d: Direction, _c: usize) -> Result<Vec<String>, Error> { Ok(Vec::new()) }
    fn set_gain(&self, _d: Direction, _c: usize, _g: f64) -> Result<(), Error> { Err(Error::NotSupported) }
    fn gain(&self, _d: Direction, _c: usize) -> Result<Option<f64>, Error> { Err(Error::NotSupported) }
    fn gain_range(&self, _d: Direction, _c: usize) -> Result<Range, Error> { Ok(empty_range()) }
    fn set_gain_element(&self, _d: Direction, _c: usize, _name: &str, _g: f64) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
    fn gain_element(&self, _d: Direction, _c: usize, _name: &str) -> Result<Option<f64>, Error> {
        Err(Error::NotSupported)
    }
    fn gain_element_range(&self, _d: Direction, _c: usize, _name: &str) -> Result<Range, Error> {
        Ok(empty_range())
    }

    fn frequency_range(&self, _d: Direction, _c: usize) -> Result<Range, Error> { Ok(empty_range()) }
    fn frequency(&self, _d: Direction, _c: usize) -> Result<f64, Error> { Err(Error::NotSupported) }
    fn set_frequency(&self, _d: Direction, _c: usize, _f: f64, _args: Args) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
    fn frequency_components(&self, _d: Direction, _c: usize) -> Result<Vec<String>, Error> { Ok(Vec::new()) }
    fn component_frequency_range(&self, _d: Direction, _c: usize, _name: &str) -> Result<Range, Error> {
        Ok(empty_range())
    }
    fn component_frequency(&self, _d: Direction, _c: usize, _name: &str) -> Result<f64, Error> {
        Err(Error::NotSupported)
    }
    fn set_component_frequency(
        &self, _d: Direction, _c: usize, _name: &str, _f: f64,
    ) -> Result<(), Error> { Err(Error::NotSupported) }

    fn sample_rate(&self, _d: Direction, _c: usize) -> Result<f64, Error> { Err(Error::NotSupported) }
    fn set_sample_rate(&self, _d: Direction, _c: usize, _rate: f64) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
    fn get_sample_rate_range(&self, _d: Direction, _c: usize) -> Result<Range, Error> {
        Ok(empty_range())
    }

    fn bandwidth(&self, _d: Direction, _c: usize) -> Result<f64, Error> { Err(Error::NotSupported) }
    fn set_bandwidth(&self, _d: Direction, _c: usize, _bw: f64) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
    fn get_bandwidth_range(&self, _d: Direction, _c: usize) -> Result<Range, Error> {
        Ok(empty_range())
    }

    fn has_dc_offset_mode(&self, _d: Direction, _c: usize) -> Result<bool, Error> { Ok(false) }
    fn set_dc_offset_mode(&self, _d: Direction, _c: usize, _auto: bool) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
    fn dc_offset_mode(&self, _d: Direction, _c: usize) -> Result<bool, Error> { Err(Error::NotSupported) }
}
