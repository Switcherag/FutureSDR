# Plugin Architecture

The plugin system allows FutureSDR blocks to be compiled as separate shared
libraries (`.so` files) and loaded at runtime via `dlopen`.

## Design Goals

1. **Same `Block` trait** — plugins produce `Box<dyn Block>`, identical to
   statically linked blocks. The runtime doesn't know or care whether a block
   came from a plugin.
2. **Minimal ABI surface** — only two C-compatible symbols are exported per
   plugin: `create_block_factory` and `plugin_abi_fingerprint`.
3. **Load-time validation** — an ABI fingerprint is checked before any code
   from the plugin executes, catching toolchain/version mismatches early.
4. **A-posteriori compilation** — a third party can build a new plugin against
   a deployed runtime without the full workspace, using the Plugin SDK.

## How It Works

```
  ┌──────────────────────┐       ┌──────────────────────┐
  │   Application Binary │       │   Plugin .so          │
  │                      │       │                       │
  │  LoadedPlugin::load()│──────→│  create_block_factory │
  │                      │  abi  │  plugin_abi_fingerprint│
  │  fg.add_block_dyn()  │ check │                       │
  │  fg.connect_dyn()    │       │  impl BlockFactory    │
  └──────┬───────────────┘       └───────┬───────────────┘
         │                               │
         │         links at runtime       │
         └───────────┬───────────────────┘
                     ▼
           ┌─────────────────┐
           │ libfuturesdr.so │
           │  (shared types) │
           └─────────────────┘
```

Both the application binary and all plugin `.so` files link against the same
`libfuturesdr.so` at runtime. This is critical: it ensures that `TypeId`
values, vtable layouts, and generic instantiations are identical across the
binary boundary.

## Compilation Model

FutureSDR builds as both `rlib` (static) and `dylib` (dynamic):

```toml
# Cargo.toml
[lib]
crate-type = ["dylib", "rlib"]
```

- **`rlib`**: Used by normal (non-plugin) builds. Everything is statically
  linked into a single binary.
- **`dylib`**: Produces `libfuturesdr.so`. Used when building plugins.

Plugins are built with:

```toml
# plugins/my_plugin/Cargo.toml
[lib]
crate-type = ["dylib"]

[dependencies]
futuresdr = { path = "../../", features = ["plugin"] }
plugin_api = { path = "../../crates/plugin_api" }
```

The `plugin` feature flag is currently a marker — it signals that this build
participates in the dynamic linking scheme. The `-C prefer-dynamic` RUSTFLAG
ensures the Rust standard library is also loaded from a shared `.so`.

## Key Crates

### `plugin_api`

The API crate that both plugins and the host binary depend on:

- **`BlockFactory` trait**: The interface plugins implement to create blocks.
- **`export_plugin!` macro**: Generates the two `#[no_mangle]` symbols that
  the loader expects.
- **`LoadedPlugin`**: The host-side loader that `dlopen`s a `.so`, validates
  the ABI fingerprint, and extracts the `BlockFactory`.

### `plugin_gen`

A code generator that reads a block's source file, detects the
`#[derive(Block)]` struct and its `new()` constructor, and generates a
complete plugin crate (Cargo.toml + src/lib.rs) with the correct
`export_plugin!` invocation.

## The BlockFactory Trait

```rust
pub trait BlockFactory: Send + Sync {
    fn create_block(&self, id: BlockId, config: Box<dyn Any + Send>) -> Box<dyn Block>;
    fn block_name(&self) -> &'static str;
    fn block_description(&self) -> &'static str;
}
```

- `create_block` receives a type-erased config (`Box<dyn Any + Send>`) and a
  `BlockId`. It downcasts the config to the expected type, constructs the
  kernel, wraps it in `WrappedKernel`, and returns `Box<dyn Block>`.
- `block_name` and `block_description` provide metadata for introspection.

## The Two Exported Symbols

Every plugin `.so` exports exactly two functions:

```rust
#[unsafe(no_mangle)]
pub fn create_block_factory() -> Box<dyn BlockFactory>

#[unsafe(no_mangle)]
pub fn plugin_abi_fingerprint() -> (&'static str, &'static str)
```

The first returns the factory. The second returns `(hash, detail)` for ABI
validation.

## Loading a Plugin

```rust
// Safe: validates ABI fingerprint, panics on mismatch
let plugin = unsafe { LoadedPlugin::load("./libmy_plugin.so") };

// Safe: returns Result instead of panicking
let plugin = unsafe { LoadedPlugin::try_load("./libmy_plugin.so") }?;

// Unsafe: skips ABI validation (development only)
let plugin = unsafe { LoadedPlugin::load_unchecked("./libmy_plugin.so") };
```

The `load` sequence:

1. `dlopen` the `.so` via `libloading::Library::new(path)`
2. Look up the `plugin_abi_fingerprint` symbol
3. Compare the plugin's fingerprint against the runtime's
   `abi_fingerprint::ABI_FINGERPRINT`
4. On mismatch, return `AbiMismatchError` with both fingerprints and their
   human-readable details
5. On match, look up `create_block_factory` and call it
6. Return `LoadedPlugin { factory, _lib }` — `_lib` is kept alive to prevent
   the OS from unloading the `.so`

## Using a Loaded Plugin

```rust
let plugin = unsafe { LoadedPlugin::load("./libhead_plugin.so") };

let mut fg = Flowgraph::new();
let src = fg.add_block(NullSource::<u8>::new());

// Create block from plugin with config
let head = fg.add_block_dyn(plugin.prepare(Box::new(12u64)));

fg.connect_dyn(src, "output", head, "input")?;
```

`prepare(config)` returns a closure `FnOnce(BlockId) -> Box<dyn Block>` that
the flowgraph calls when assigning the block its ID.

## Dynamic Connections

Plugin-created blocks use `connect_dyn` instead of `connect_stream`:

```rust
fg.connect_dyn(src_id, "output", dst_id, "input")?;
```

This resolves ports by name at runtime and uses `Any::downcast_mut` to verify
type compatibility. If the buffer reader's concrete type doesn't match the
writer's expected reader type, the connection fails with an error rather than
causing undefined behavior.
