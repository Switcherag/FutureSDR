# ABI Contract & Fingerprint

Dynamic plugins share a binary interface with the runtime. If any part of that
interface changes between compilation of the runtime and the plugin, the result
is undefined behavior — usually silent memory corruption. This chapter
explains the ABI contract and the fingerprint mechanism that enforces it.

## What Crosses the ABI Boundary

| Component | Crosses? | Why |
|-----------|----------|-----|
| `Block` trait vtable | Yes | Plugin returns `Box<dyn Block>` |
| `BufferReader` / `BufferWriter` | Yes | `connect_dyn` downcasts via `TypeId` |
| `Pmt` enum layout | Yes | Messages flow between blocks |
| `Tag` enum layout | Yes | Tags travel with buffer data |
| `Error` enum layout | Yes | Returned from handlers |
| `BlockMessage` / `FlowgraphMessage` | Yes | Sent via channels |
| `BlockFactory` trait vtable | Yes | Plugin exports `Box<dyn BlockFactory>` |
| Plugin-internal types | No | Stay inside `work()` / handlers |
| External crate types (internal) | No | Never cross the `.so` boundary |

## The ABI Contract

For a plugin to be binary-compatible with the runtime, **all** of the
following must match:

1. **Rust toolchain** — exact nightly version (including commit hash). Rust's
   nightly ABI can change between any two versions.

2. **FutureSDR version** — the `Cargo.toml` version string. Any change to
   public types, enum variants, or trait signatures breaks compatibility.

3. **Target triple** — e.g., `x86_64-unknown-linux-gnu`. Cross-platform
   plugins are not supported.

4. **RUSTFLAGS** — flags like `-C prefer-dynamic`, `-C panic=unwind`,
   `-C lto=off` affect code generation and must match.

5. **ABI-affecting features** — features that change enum layouts. Currently,
   only `seify` was historically relevant, but the `Error` enum has been fixed
   to have a stable layout regardless of features (see below).

## The Error Enum Fix

The `Error` enum previously had `#[cfg(feature = "seify")]` on two variants:

```rust
// BEFORE (broken): variant positions shift based on features
pub enum Error {
    // ...
    #[cfg(feature = "seify")]
    SeifyArgsConversionError,
    #[cfg(feature = "seify")]
    SeifyError(String),
}
```

If the runtime was built with `seify` but a plugin was built without it (or
vice versa), the enum discriminant values would differ, causing silent memory
corruption when `Error` values crossed the boundary.

The fix: both variants are now always present, regardless of features. Only the
`From<seify::Error>` impl is feature-gated:

```rust
// AFTER (safe): layout is identical regardless of features
#[non_exhaustive]
pub enum Error {
    // ...
    SeifyArgsConversionError,
    SeifyError(String),
}

#[cfg(feature = "seify")]
impl From<seify::Error> for Error { /* ... */ }
```

## ABI Fingerprint

The fingerprint is a compile-time hash that captures all ABI-affecting
parameters. It is generated in `build.rs` and embedded in both the runtime
and every plugin.

### Generation (build.rs)

```rust
let fingerprint_source = format!(
    "rustc={};futuresdr={};target={};seify={}",
    rustc_version,
    futuresdr_version,
    target,
    has_seify_feature,
);

let mut hasher = DefaultHasher::new();
fingerprint_source.hash(&mut hasher);
let hash = format!("{:016x}", hasher.finish());
```

Two environment variables are emitted:

- `FUTURESDR_ABI_FINGERPRINT` — 16-character hex hash (for fast comparison)
- `FUTURESDR_ABI_FINGERPRINT_DETAIL` — human-readable source string (for
  error messages)

### Runtime Side

```rust
// src/runtime/abi_fingerprint.rs
pub const ABI_FINGERPRINT: &str = env!("FUTURESDR_ABI_FINGERPRINT");
pub const ABI_FINGERPRINT_DETAIL: &str = env!("FUTURESDR_ABI_FINGERPRINT_DETAIL");
```

### Plugin Side

The `export_plugin!` macro generates:

```rust
#[unsafe(no_mangle)]
pub fn plugin_abi_fingerprint() -> (&'static str, &'static str) {
    (
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT,
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT_DETAIL,
    )
}
```

Since both the runtime and the plugin link against `libfuturesdr.so`, and the
constants are baked in at compile time, the fingerprints will match if and only
if they were built from the same `libfuturesdr.so`.

### Validation at Load Time

`LoadedPlugin::try_load()` performs this check:

```rust
let fp_fn: Symbol<AbiFingerPrintFn> = lib.get(b"plugin_abi_fingerprint")?;
let (plugin_hash, plugin_detail) = fp_fn();

if plugin_hash != abi_fingerprint::ABI_FINGERPRINT {
    return Err(AbiMismatchError {
        plugin_path: path.into(),
        plugin_fingerprint: plugin_hash.into(),
        plugin_detail: plugin_detail.into(),
        runtime_fingerprint: abi_fingerprint::ABI_FINGERPRINT.into(),
        runtime_detail: abi_fingerprint::ABI_FINGERPRINT_DETAIL.into(),
    }.into());
}
```

### Error Message

On mismatch, you get a clear diagnostic:

```
ABI mismatch loading plugin `./libmy_plugin.so`:
  plugin:  a1b2c3d4e5f67890 (rustc=1.84.0-nightly(abc1234);futuresdr=0.1.0;target=x86_64-unknown-linux-gnu;seify=false)
  runtime: f0e1d2c3b4a59687 (rustc=1.85.0-nightly(def5678);futuresdr=0.1.0;target=x86_64-unknown-linux-gnu;seify=false)
```

This tells you exactly what differs (in this example, the Rust toolchain
version).

## Toolchain Pinning

To prevent accidental toolchain drift, the workspace includes a
`rust-toolchain.toml`:

```toml
[toolchain]
channel = "nightly-2025-11-18"
components = ["rustfmt", "clippy"]
```

Anyone building in the workspace directory automatically uses this exact
nightly. Plugins built outside the workspace must use the same version.

## `load_unchecked`

For development iteration where you're rebuilding everything together and
don't want the fingerprint check overhead:

```rust
let plugin = unsafe { LoadedPlugin::load_unchecked("./libmy_plugin.so") };
```

This skips the fingerprint symbol lookup entirely. Use this only during
development — never in production.

## Practical Implications

### What Triggers a Fingerprint Mismatch

| Change | Mismatch? |
|--------|-----------|
| `rustup update` (new nightly) | Yes |
| Edit a block's `work()` logic | No (rebuild both) |
| Add a new `Pmt` variant | Yes (version bump) |
| Change RUSTFLAGS | Yes |
| Build on a different machine (same toolchain) | No |
| Build plugin without `seify` feature | No (layout is stable) |

### Recommended Workflow

1. Pin the toolchain with `rust-toolchain.toml`
2. Build everything together with `setup_plugins.sh`
3. Use `LoadedPlugin::load()` (not `load_unchecked`) in production
4. When distributing plugins separately, use the Plugin SDK (next chapter)
