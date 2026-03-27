# Writing a Plugin

This chapter walks through creating a plugin from scratch and using the
`plugin_gen` code generator.

## Quick Start: The `export_plugin!` Macro

The simplest plugin wraps an existing FutureSDR block:

```rust
// plugins/head_plugin/src/lib.rs
use futuresdr::blocks::Head;

plugin_api::export_plugin! {
    name: "Head",
    description: "Head block plugin",
    config: u64,
    create: |cfg, _id| {
        Head::<u8>::new(cfg)
    }
}
```

The macro parameters:

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | `&str` | Block name for introspection |
| `description` | `&str` | Human-readable description |
| `config` | type | The concrete type the caller passes in `Box<dyn Any>` |
| `create` | closure | `\|config: ConfigType, id: BlockId\| -> KernelType` |

The macro generates:

1. A `__PluginFactory` struct implementing `BlockFactory`
2. `create_block_factory()` — the `#[no_mangle]` entry point
3. `plugin_abi_fingerprint()` — the ABI validation symbol

## Plugin Cargo.toml

```toml
[package]
name = "head_plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["dylib"]

[dependencies]
plugin_api = { path = "../../crates/plugin_api" }
futuresdr = { path = "../../", features = ["plugin"] }
```

Key requirements:

- `crate-type = ["dylib"]` — produces a `.so` file
- `features = ["plugin"]` — participates in the dynamic linking scheme
- Both `plugin_api` and `futuresdr` as dependencies

## Config Types

The config is passed as `Box<dyn Any + Send>` and downcast inside the factory.
Choose the type based on what your block's constructor needs:

### No Config

```rust
plugin_api::export_plugin! {
    name: "Increment",
    description: "Increments each sample by one",
    config: (),
    create: |_cfg, _id| {
        Increment::<f32>::new()
    }
}
```

The caller passes `Box::new(())`.

### Scalar Config

```rust
config: u64,
create: |n_items, _id| {
    Head::<u8>::new(n_items)
}
```

The caller passes `Box::new(12u64)`.

### Tuple Config

```rust
config: (f32, usize),
create: |(cutoff, n_taps), _id| {
    LowPassFilter::new(cutoff, n_taps)
}
```

The caller passes `Box::new((0.25f32, 64usize))`.

### String Config (for type selection)

When a plugin supports multiple sample types:

```rust
config: String,
create: |type_name, _id| {
    match type_name.as_str() {
        "f32"  => NullSink::<f32>::new(),
        "u8"   => NullSink::<u8>::new(),
        "c32"  => NullSink::<Complex32>::new(),
        _ => panic!("unsupported type: {type_name}"),
    }
}
```

## Custom Blocks in Plugins

You can define a block directly inside a plugin crate:

```rust
// plugins/increment_plugin/src/lib.rs
use futuresdr::macros::Block;
use futuresdr::runtime::*;
use futuresdr::runtime::buffer::*;

#[derive(Block)]
pub struct Increment<
    T: Send + Sync + 'static + std::ops::AddAssign + From<u8>,
    I: CpuBufferReader<Item = T> = DefaultCpuReader<T>,
    O: CpuBufferWriter<Item = T> = DefaultCpuWriter<T>,
> {
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<T, I, O> Increment<T, I, O>
where
    T: Send + Sync + 'static + Clone + std::ops::AddAssign + From<u8>,
    I: CpuBufferReader<Item = T>,
    O: CpuBufferWriter<Item = T>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            output: O::default(),
        }
    }
}

impl<T, I, O> Kernel for Increment<T, I, O>
where
    T: Send + Sync + 'static + Clone + std::ops::AddAssign + From<u8>,
    I: CpuBufferReader<Item = T>,
    O: CpuBufferWriter<Item = T>,
{
    async fn work(
        &mut self, io: &mut WorkIo, _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let m = std::cmp::min(i.len(), o.len());
        if m > 0 {
            for idx in 0..m {
                o[idx] = i[idx].clone();
                o[idx] += T::from(1u8);
            }
            self.input.consume(m);
            self.output.produce(m);
        }
        if self.input.finished() && m == i.len() {
            io.finished = true;
        }
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "Increment",
    description: "Adds 1 to each sample",
    config: (),
    create: |_cfg, _id| {
        Increment::<f32>::new()
    }
}
```

## Generic Plugins (Manual BlockFactory)

When `export_plugin!` isn't flexible enough (e.g., multiple sample types
selected at runtime), implement `BlockFactory` manually:

```rust
use futuresdr::runtime::*;
use plugin_api::BlockFactory;

struct NullSinkFactory;

impl BlockFactory for NullSinkFactory {
    fn create_block(&self, id: BlockId, config: Box<dyn std::any::Any + Send>) -> Box<dyn Block> {
        let type_name = *config.downcast::<String>().expect("config must be String");
        match type_name.as_str() {
            "f32" => {
                let kernel = futuresdr::blocks::NullSink::<f32>::new();
                Box::new(WrappedKernel::new(kernel, id))
            }
            "c32" => {
                let kernel = futuresdr::blocks::NullSink::<num_complex::Complex32>::new();
                Box::new(WrappedKernel::new(kernel, id))
            }
            _ => panic!("unsupported type: {type_name}"),
        }
    }

    fn block_name(&self) -> &'static str { "NullSink" }
    fn block_description(&self) -> &'static str { "Generic null sink" }
}

#[unsafe(no_mangle)]
pub fn create_block_factory() -> Box<dyn BlockFactory> {
    Box::new(NullSinkFactory)
}

#[unsafe(no_mangle)]
pub fn plugin_abi_fingerprint() -> (&'static str, &'static str) {
    (
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT,
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT_DETAIL,
    )
}
```

When implementing `BlockFactory` manually, you **must** also export
`plugin_abi_fingerprint` yourself.

## Using `plugin_gen`

The `plugin_gen` tool automates plugin crate creation:

```bash
cargo run -p plugin_gen -- --block-src src/blocks/head.rs
```

It:

1. Parses the source file for `#[derive(Block)]` structs
2. Extracts the first generic type parameter (for monomorphization)
3. Finds `pub fn new(...)` and maps parameter types to plugin-compatible types
4. Generates `plugins/<block_name>_plugin/Cargo.toml` and `src/lib.rs`

### Type Mapping Rules

| Constructor type | Plugin config type |
|------------------|-------------------|
| `()` (no params) | `()` |
| `u64`, `f32`, etc. | Same type |
| `&str`, `impl AsRef<str>` | `String` |
| `impl AsRef<Path>` | `String` |
| `&[T]` | `Vec<T>` |
| `Vec<T>` | `Vec<T>` |
| `(A, B, ...)` multiple params | `(A, B, ...)` tuple |
| `impl FnMut(...)` | `CLOSURE` (manual editing required) |

When the generator encounters a closure parameter, it emits a `CLOSURE`
placeholder and a comment at the top of the file telling you to replace it
manually.

## Building Plugins

Plugins must be built with matching RUSTFLAGS:

```bash
export RUSTFLAGS="-C prefer-dynamic -C panic=unwind -C lto=off"

# Build futuresdr first (produces libfuturesdr.so)
cargo build --release

# Build the plugin
cargo build --release -p head_plugin
```

The `setup_plugins.sh` script automates this for the entire workspace.

## Runtime Usage

```rust
use plugin_api::LoadedPlugin;

let plugin = unsafe { LoadedPlugin::load("./shared_libs/release/libhead_plugin.so") };
println!("Loaded: {}", plugin.block_name());

let mut fg = Flowgraph::new();
let head = fg.add_block_dyn(plugin.prepare(Box::new(100u64)));
```

Set `LD_LIBRARY_PATH` to include the directory containing `libfuturesdr.so`
and `libstd-*.so`:

```bash
export LD_LIBRARY_PATH=./shared_libs/release:$LD_LIBRARY_PATH
./target/release/my_app
```
