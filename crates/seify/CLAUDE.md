# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Seify is a Rust SDR (Software-Defined Radio) hardware abstraction library providing a unified interface for various SDR hardware devices. It is part of the FutureSDR project.

## Common Commands

```bash
# Format check (uses nightly)
cargo fmt --all -- --check

# Lint (CI uses these features)
cargo clippy --all-targets --workspace --features=rtlsdr,aaronia_http,soapy -- -D warnings

# Run tests
cargo test --all-targets --features=aaronia_http,rtlsdr,soapy

# Run a single test
cargo test --features=rtlsdr <test_name>
```

**Note:** CI uses the nightly toolchain. The `soapy` feature requires `libsoapysdr-dev` installed on the system.

## Available Features

- `soapy` - SoapySDR wrapper (default)
- `dummy` - Dummy driver for testing (default)
- `rtlsdr` - Pure Rust RTL-SDR driver (sub-crate `seify-rtlsdr` / `crates/rtl-sdr-rs`)
- `hackrfone` - HackRF One native Rust driver (sub-crate `seify-hackrfone` / `crates/seify-hackrfone`)
- `aaronia_http` - Aaronia HTTP backend

## Architecture

### Two-Tier Device Interface

The central design pattern: all hardware drivers implement `DeviceTrait`, and `Device<T>` wraps them in two modes:

- **Typed** (`Device<T: DeviceTrait>`): Zero-cost, compile-time known types. Use `Device::from_impl(driver_instance)`.
- **Generic** (`Device<GenericDevice>`): Type-erased via `Arc<dyn DeviceTrait + Sync>` with boxed streamers. Suitable when the driver type is unknown at compile time.

`DeviceWrapper<D>` bridges typed drivers into the generic interface by boxing their concrete streamer types.

### Core Traits (src/device.rs, src/streamer.rs)

- **`DeviceTrait`**: ~40 methods covering antenna control, AGC, gain, frequency, sample rate, bandwidth, DC offset, and RX/TX streaming. Has associated types `RxStreamer` and `TxStreamer`.
- **`RxStreamer` / `TxStreamer`**: Provide `mtu()`, `activate()`, `deactivate()`, `read()` / `write()` with timeout support.

### Driver Implementations (src/impls/)

Each driver is feature-gated:

| Driver | Feature | Notes |
|--------|---------|-------|
| `rtlsdr.rs` | `rtlsdr` | Pure Rust via `rusb` |
| `soapy.rs` | `soapy` | Wraps SoapySDR C++ library |
| `hackrfone.rs` | `hackrfone` | Pure Rust via `nusb` (async-based) |
| `aaronia_http.rs` | `aaronia_http` | HTTP backend |
| `aaronia.rs` | `aaronia` | Direct Aaronia backend |
| `dummy.rs` | `dummy` | Test/CI dummy device |

### Supporting Types

- **`Args`** (`src/args.rs`): Key-value config parsed from `"key=value, key2=value2"` strings using `nom`. Used for device enumeration and streamer configuration.
- **`Range` / `RangeItem`** (`src/range.rs`): Constraint types — `Interval(min, max)`, `Value(v)`, `Step(min, max, step)`. Used to report valid parameter ranges from devices.
- **`Error`** (`src/error.rs`): Custom enum with feature-gated variants per backend.

### Workspace Layout

```
seify/                  # Main crate (lib)
  src/
    device.rs           # DeviceTrait, Device<T>, GenericDevice, DeviceWrapper
    streamer.rs         # RxStreamer, TxStreamer traits
    impls/              # Per-driver implementations (feature-gated)
    args.rs, range.rs, error.rs
  examples/             # Usage examples (rx_typed.rs, rx_generic.rs, etc.)
crates/
  rtl-sdr-rs/           # seify-rtlsdr sub-crate (GPL-3.0)
  seify-hackrfone/      # seify-hackrfone sub-crate (MIT)
```

### Downcasting Pattern

Generic devices can be downcast back to concrete types via `device.impl_ref::<ConcreteType>()` and `device.impl_mut::<ConcreteType>()`, which use the `Any` bound on `DeviceTrait`.
