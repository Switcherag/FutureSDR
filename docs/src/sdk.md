# A-Posteriori Compilation & SDK

A-posteriori compilation means building a plugin *after* the runtime has been
deployed, without the full FutureSDR workspace. The Plugin SDK makes this
practical.

## The Problem

To build a compatible plugin, you need:

1. The exact Rust nightly toolchain
2. The exact FutureSDR source (or at least the dylib)
3. The same RUSTFLAGS
4. The same ABI-affecting feature configuration

Without coordination, getting all four right is fragile.

## The Plugin SDK

The `setup_plugins.sh` script packages everything needed into a self-contained
`plugin_sdk/` directory:

```
plugin_sdk/
├── libfuturesdr.so              # The exact runtime binary
├── libstd-*.so                  # Matching nightly stdlib
├── rust-toolchain.toml          # Exact nightly version
├── build.env                    # RUSTFLAGS
├── plugin_template_Cargo.toml   # Template for new plugin crates
└── manifest.json                # ABI metadata
```

### Contents

**`libfuturesdr.so`** — the release build of the FutureSDR dynamic library.
This is the binary your plugin will link against at runtime.

**`libstd-*.so`** — the Rust standard library from the pinned nightly. Needed
because `-C prefer-dynamic` makes the binary depend on the stdlib `.so`.

**`rust-toolchain.toml`** — pins the exact nightly version:

```toml
[toolchain]
channel = "nightly-2025-11-18"
components = ["rustfmt", "clippy"]
```

**`build.env`** — the RUSTFLAGS used to build the runtime:

```bash
export RUSTFLAGS="-C prefer-dynamic -C panic=unwind -C lto=off"
```

**`plugin_template_Cargo.toml`** — a template for new plugin crates:

```toml
[package]
name = "my_plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["dylib"]

[dependencies]
plugin_api = { path = "path/to/crates/plugin_api" }
futuresdr = { path = "path/to/futuresdr", features = ["plugin"] }
```

**`manifest.json`** — machine-readable metadata:

```json
{
    "rustc_version": "1.84.0-nightly",
    "futuresdr_version": "0.1.0",
    "target": "x86_64-unknown-linux-gnu",
    "rustflags": "-C prefer-dynamic -C panic=unwind -C lto=off",
    "created": "2025-11-20T14:30:00Z"
}
```

## Third-Party Plugin Development Workflow

### Step 1: Install the Pinned Toolchain

```bash
# Copy rust-toolchain.toml to your plugin directory
cp plugin_sdk/rust-toolchain.toml ./

# Rustup will install the correct nightly automatically
rustc --version  # Verify it matches manifest.json
```

### Step 2: Create the Plugin Crate

```bash
cargo init --lib my_plugin
```

Edit `Cargo.toml` based on the template:

```toml
[lib]
crate-type = ["dylib"]

[dependencies]
plugin_api = { path = "/path/to/crates/plugin_api" }
futuresdr = { path = "/path/to/futuresdr", features = ["plugin"] }
```

### Step 3: Write the Plugin

```rust
// src/lib.rs
use futuresdr::blocks::Apply;
use futuresdr::num_complex::Complex32;

plugin_api::export_plugin! {
    name: "MyFilter",
    description: "Custom frequency-domain filter",
    config: f32,
    create: |cutoff, _id| {
        Apply::<Complex32, Complex32, _, _>::new(move |x: &Complex32| {
            if x.norm() > cutoff { *x } else { Complex32::new(0.0, 0.0) }
        })
    }
}
```

### Step 4: Build with Matching Flags

```bash
source plugin_sdk/build.env  # Sets RUSTFLAGS
cargo build --release
```

### Step 5: Deploy

Copy the resulting `.so` to the target system alongside `libfuturesdr.so` and
`libstd-*.so`:

```bash
cp target/release/libmy_plugin.so /deploy/shared_libs/
```

The runtime validates the ABI fingerprint at load time:

```rust
// This will succeed if everything matches
let plugin = unsafe { LoadedPlugin::load("/deploy/shared_libs/libmy_plugin.so") };
```

If the toolchain or FutureSDR version doesn't match, you get a clear error
message instead of silent corruption.

## Generating the SDK

The SDK is generated as part of the `setup_plugins.sh` build:

```bash
./setup_plugins.sh
# SDK is created at ./plugin_sdk/
```

The relevant section of the script:

1. Creates the `plugin_sdk/` directory
2. Copies `libfuturesdr.so` from `target/release/`
3. Copies `libstd-*.so` from the rustup sysroot
4. Copies `rust-toolchain.toml` from the workspace root
5. Writes `build.env` with the current RUSTFLAGS
6. Writes `plugin_template_Cargo.toml` with correct dependency paths
7. Writes `manifest.json` with version metadata and timestamp

## Verification

After building a third-party plugin:

```rust
// This validates the ABI fingerprint
match unsafe { LoadedPlugin::try_load("./libmy_plugin.so") } {
    Ok(plugin) => println!("Plugin loaded: {}", plugin.block_name()),
    Err(e) => eprintln!("Load failed: {e}"),
}
```

Common failure modes:

| Symptom | Cause | Fix |
|---------|-------|-----|
| "ABI mismatch" with different rustc | Wrong nightly | Install from `rust-toolchain.toml` |
| "ABI mismatch" with different futuresdr | Source drift | Rebuild against SDK's `libfuturesdr.so` |
| `dlopen` fails: "libfuturesdr.so not found" | Missing library path | Set `LD_LIBRARY_PATH` |
| `dlopen` fails: "libstd not found" | Missing stdlib | Copy `libstd-*.so` to library path |
| "dyn BufferReader has wrong type" | RUSTFLAGS mismatch | Source `build.env` before building |

## Limitations

- Plugins cannot use Rust stable — nightly is required for the `dylib`
  crate type to produce ABI-compatible output.
- The SDK assumes the same target triple. Cross-compilation of plugins is
  not supported.
- LTO must be disabled (`-C lto=off`) for dynamic linking to work correctly.
- Each plugin SDK is tied to a specific FutureSDR build. Updating the runtime
  requires regenerating the SDK and rebuilding all third-party plugins.
