# Troubleshooting

Common issues when working with FutureSDR plugins and how to resolve them.

## "ABI mismatch loading plugin"

**Full error**:
```
ABI mismatch loading plugin `./libmy_plugin.so`:
  plugin:  a1b2c3d4e5f67890 (rustc=1.84.0-nightly(abc);futuresdr=0.1.0;target=x86_64-unknown-linux-gnu;seify=false)
  runtime: f0e1d2c3b4a59687 (rustc=1.85.0-nightly(def);futuresdr=0.1.0;target=x86_64-unknown-linux-gnu;seify=false)
```

**Cause**: The plugin and runtime were compiled with different parameters.
Compare the detail strings to identify what differs:

| Field differs | Fix |
|---------------|-----|
| `rustc` version | Rebuild plugin with the same nightly (use `rust-toolchain.toml`) |
| `futuresdr` version | Rebuild against the same FutureSDR source |
| `target` | Build for the same platform |
| `seify` | Rebuild with matching features |

**Quick fix during development**: Use `LoadedPlugin::load_unchecked()` to skip
the check. Do not use this in production.

## "dyn BufferReader has wrong type"

**Cause**: `connect_dyn` failed because the writer's expected reader type
doesn't match the actual reader type. This happens when:

1. **Sample type mismatch**: Source produces `f32`, sink expects `Complex32`.
   Fix: ensure both sides use the same sample type.

2. **Different RUSTFLAGS**: The plugin was built with different compiler flags
   than the runtime. Even with the same source code, different flags can
   produce different `TypeId` values. Fix: source `build.env` before building.

3. **Different compiler version**: Similar to RUSTFLAGS — `TypeId` is not
   stable across compiler versions. Fix: use `rust-toolchain.toml`.

4. **Different buffer backends**: Source uses `circular::Writer<f32>` but sink
   expects `slab::Reader<f32>`. This can happen if one was built for WASM and
   the other for native. Fix: ensure consistent platform targeting.

## "libfuturesdr.so: cannot open shared object file"

**Cause**: The dynamic linker can't find `libfuturesdr.so`.

**Fix**:
```bash
export LD_LIBRARY_PATH=/path/to/shared_libs:$LD_LIBRARY_PATH
```

Or copy the `.so` files to a system library path and run `ldconfig`.

## "libstd-*.so: cannot open shared object file"

**Cause**: The Rust standard library `.so` is missing. This happens when
building with `-C prefer-dynamic`.

**Fix**: Copy `libstd-*.so` from the rustup sysroot to your library path:

```bash
# Find it
find ~/.rustup -name "libstd-*.so" | grep nightly

# Copy it
cp ~/.rustup/toolchains/nightly-*/lib/libstd-*.so ./shared_libs/
```

## Plugin config downcast panic

**Error**:
```
thread 'main' panicked at 'config must be ...'
```

**Cause**: The `Box<dyn Any>` passed to `prepare()` contains the wrong type.
For example, the plugin expects `u64` but you passed `String`.

**Fix**: Check the plugin's `export_plugin!` config type and pass the correct
type:

```rust
// Plugin expects: config: u64
plugin.prepare(Box::new(100u64))  // correct
plugin.prepare(Box::new("100"))   // WRONG: &str, not u64
```

## "symbol not found: create_block_factory"

**Cause**: The `.so` file doesn't export the expected symbol. This can happen
if:

1. The plugin doesn't use `export_plugin!` or manually export the symbol
2. The `.so` is not a FutureSDR plugin (wrong file)
3. Symbol stripping removed the export

**Fix**: Verify the `.so` exports the symbol:

```bash
nm -D libmy_plugin.so | grep create_block_factory
```

## "symbol not found: plugin_abi_fingerprint"

**Cause**: The plugin was built with an older version of `plugin_api` that
doesn't generate the fingerprint symbol.

**Fix**: Rebuild the plugin with the current `plugin_api`. If using manual
`BlockFactory` implementation, add the fingerprint export:

```rust
#[unsafe(no_mangle)]
pub fn plugin_abi_fingerprint() -> (&'static str, &'static str) {
    (
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT,
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT_DETAIL,
    )
}
```

Note: if the symbol is missing, `LoadedPlugin::load()` and `try_load()` will
still load the plugin (the fingerprint check is skipped when the symbol isn't
found). However, you lose the safety guarantee.

## Blocks finish immediately

**Cause**: A block's `work()` sets `io.finished = true` on the first call
because the input buffer is empty and `self.input.finished()` returns true.

This often happens when the upstream block hasn't produced any data yet. Check
the shutdown condition:

```rust
// WRONG: finishes before any data arrives
if self.input.finished() {
    io.finished = true;
}

// CORRECT: only finish when all data has been consumed
if self.input.finished() && self.input.slice().is_empty() {
    io.finished = true;
}
```

## Message handler not called

**Cause**: The message connection is missing or the port name is wrong.

**Check**:
1. `fg.connect_message(src, "port_name", dst, "handler_name")?` — port names
   must match exactly.
2. The handler method name must match the name in `#[message_inputs(...)]`.
3. Rust reserved words need `r#` prefix: `#[message_inputs(r#in)]` →
   `async fn r#in(...)`.

## SEGFAULT or memory corruption

If you get a segfault when loading or using a plugin:

1. **Check ABI compatibility**: Use `LoadedPlugin::try_load()` instead of
   `load()` to get a proper error instead of undefined behavior.
2. **Check RUSTFLAGS match**: Different optimization levels or codegen options
   can cause subtle ABI differences.
3. **Check for stale `.so` files**: After rebuilding, make sure you're loading
   the new `.so`, not a cached copy.
4. **Run with `RUST_BACKTRACE=1`**: Get a stack trace to identify the crash
   location.

## Build errors

### "can't find crate for `std`"

You're building a `dylib` crate without `-C prefer-dynamic`. Add it to
RUSTFLAGS:

```bash
export RUSTFLAGS="-C prefer-dynamic -C panic=unwind -C lto=off"
```

### "multiple versions of crate `futuresdr`"

A plugin's `Cargo.toml` points to a different FutureSDR path or version than
the main binary. Ensure all workspace members reference the same path.

### LTO errors

Dynamic libraries cannot use LTO. Ensure `-C lto=off` is in RUSTFLAGS.
