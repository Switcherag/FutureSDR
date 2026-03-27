# FutureSDR Plugin API

Dynamic plugin system for loading FutureSDR blocks at runtime from shared libraries (`.so` files).

## Quick Start

```rust
use futuresdr::prelude::*;

plugin_api::export_plugin! {
    name: "MyBlock",
    description: "Does something useful",
    config: u64,
    create: |cfg, _id| {
        MyBlock::new(cfg)
    }
}
```

Build with:
```bash
export RUSTFLAGS="-C prefer-dynamic -C panic=unwind -C lto=off"
cargo build --release -p my_plugin
```

Load at runtime:
```rust
let plugin = unsafe { LoadedPlugin::load("./libmy_plugin.so") };
let block_id = fg.add_block_dyn(plugin.prepare(Box::new(42u64)));
```

## ABI Contract

Rust does not have a stable ABI. Plugins and the runtime must be compiled under
identical conditions for binary compatibility. The following must all match:

| Constraint | Why |
|---|---|
| **Rust toolchain** (exact nightly version) | Struct layout, vtable layout, calling conventions can change between nightlies |
| **FutureSDR version** | Trait definitions, enum variants, type layouts |
| **Target triple** | Obviously |
| **RUSTFLAGS** | `-C prefer-dynamic` is required for shared generic type instantiations |
| **ABI-affecting features** | `seify` was historically problematic (fixed: Error enum variants are now always present) |

### ABI Fingerprint

Every plugin and the runtime embed a compile-time ABI fingerprint (a hash of
rustc version + futuresdr version + target + features). `LoadedPlugin::load()`
validates this fingerprint at load time and produces a clear error message on
mismatch instead of silent memory corruption.

To skip validation (e.g., during development):
```rust
let plugin = unsafe { LoadedPlugin::load_unchecked("./libmy_plugin.so") };
```

## External Libraries in Plugins

**Plugins CAN use external crates.** The rule is simple:

### Safe (types stay inside the plugin)

Any crate whose types never cross the ABI boundary:

- Computation libraries: `nalgebra`, `ndarray`, `rayon`
- Serialization: `serde`, `serde_json`, `bincode`
- Utilities: `crc32fast`, `regex`, `rand`
- I/O: `tokio`, `reqwest` (for internal async work)

These are statically linked into the plugin `.so` and invisible to the runtime.

**Example:** `wlan_mac_plugin` uses `crc32fast` for CRC computation. The CRC
result is a `u32` that gets embedded into a `Vec<u8>` frame — only the `Vec<u8>`
(via `Pmt::Blob`) crosses the ABI boundary.

### Unsafe (types cross the ABI boundary)

Types that flow through:
- **Stream buffers** — the `D` in `circular::Reader<D>` / `circular::Writer<D>`
- **`Pmt::Any`** — if one plugin puts a value in and a different plugin downcasts it
- **`Tag::NamedAny`** — same as above

These types MUST come from `libfuturesdr.so` (i.e., be dependencies of the
futuresdr crate) so that their `TypeId` is shared across all plugins.

### When to add a dependency to futuresdr

Only if you need a new **buffer sample type** (the `D` in `Reader<D>`). For
example, if you wanted to stream `nalgebra::Vector3<f32>` between blocks,
`nalgebra` would need to be a futuresdr dependency.

Standard types already available:
- `u8`, `f32`, `u32`, `i16`, etc. (primitives)
- `Complex32` (from `num_complex`, re-exported by futuresdr)

**You do NOT need to pre-import every possible library into futuresdr.**

## Build Order: Unified Builds

When building plugins and binaries from the same workspace, **always build them
in the same `cargo build` command**:

```bash
cargo build --release -p my_plugin -p my_binary
```

Cargo unifies feature flags across a single invocation. If you build plugins
and the binary separately, `libfuturesdr.so` may be recompiled with different
features between the two builds, causing symbol mismatches at runtime.

## A-Posteriori Plugin Compilation

Plugins can be compiled after the runtime is deployed, as long as the
ABI contract is satisfied:

1. Use the exact Rust nightly from `rust-toolchain.toml`
2. Use the same futuresdr source (or the Plugin SDK)
3. Build with `RUSTFLAGS="-C prefer-dynamic -C panic=unwind -C lto=off"`
4. The runtime validates the ABI fingerprint at load time

### Plugin SDK

Run `source setup_plugins.sh` to generate a `plugin_sdk/` directory containing:
- `libfuturesdr.so` — the exact runtime binary
- `libstd-*.so` — matching Rust stdlib
- `rust-toolchain.toml` — pinned toolchain
- `build.env` — required RUSTFLAGS
- `plugin_template_Cargo.toml` — template for new plugins
- `manifest.json` — ABI metadata (versions, fingerprint)

## Troubleshooting

### "dyn BufferReader has wrong type"
The `TypeId` of a buffer type (e.g., `circular::Reader<Complex32>`) does not
match between the writer's plugin and the reader's plugin. Causes:
- **Wrong sample type**: e.g., `NullSink::<u8>` connected to a `Complex32` output.
  Fix: use the correct generic type parameter.
- **Different RUSTFLAGS**: one plugin built without `-C prefer-dynamic`.
  Fix: rebuild all plugins with the same RUSTFLAGS.
- **Different compiler version**: plugins built with different nightlies.
  Fix: use `rust-toolchain.toml` to pin the toolchain.

### "ABI mismatch loading plugin"
The plugin's compile-time fingerprint does not match the runtime's. The error
message shows both fingerprints with their detail strings. Fix: rebuild the
plugin against the same futuresdr version with the same toolchain.

### "Seify Args conversion error" at runtime
Legacy issue from before the Error enum fix. If you see this with current code,
ensure both plugin and runtime are rebuilt from the same source.

### Plugin loads but panics on config downcast
The config type passed to `plugin.prepare(Box::new(...))` does not match
what the plugin expects. Check the plugin's `export_plugin!` `config:` type.

## Generic Plugins (Multiple Types)

If a plugin needs to support multiple sample types, implement `BlockFactory`
manually instead of using `export_plugin!`:

```rust
use futuresdr::runtime::{Block, BlockId, WrappedKernel};

struct MyFactory;

impl plugin_api::BlockFactory for MyFactory {
    fn create_block(&self, id: BlockId, config: Box<dyn Any + Send>) -> Box<dyn Block> {
        if let Ok(type_name) = config.downcast::<String>() {
            match type_name.as_str() {
                "c32" => Box::new(WrappedKernel::new(MyBlock::<Complex32>::new(), id)),
                _ => Box::new(WrappedKernel::new(MyBlock::<u8>::new(), id)),
            }
        } else {
            Box::new(WrappedKernel::new(MyBlock::<u8>::new(), id))
        }
    }
    fn block_name(&self) -> &'static str { "MyBlock" }
    fn block_description(&self) -> &'static str { "..." }
}

#[unsafe(no_mangle)]
pub fn create_block_factory() -> Box<dyn plugin_api::BlockFactory> {
    Box::new(MyFactory)
}

// Don't forget the ABI fingerprint when not using export_plugin!
#[unsafe(no_mangle)]
pub fn plugin_abi_fingerprint() -> (&'static str, &'static str) {
    (
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT,
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT_DETAIL,
    )
}
```
