# External Libraries in Plugins

Plugins can use external crates (crates.io dependencies). The key question is
whether any types from those crates cross the ABI boundary.

## The Rule

> If a type stays entirely inside your plugin's `work()`, `init()`, `deinit()`,
> or message handler methods, it is **safe** to use any crate. If a type flows
> through buffers (`Reader<T>` / `Writer<T>`), `Pmt::Any`, or `Tag::NamedAny`
> to another block, it must come from `libfuturesdr.so`.

## Safe: Types Stay Internal

These patterns are always safe because the external types never cross the
`.so` boundary:

### Computation Libraries

```rust
// Inside work()
use nalgebra::Matrix4;
let transform = Matrix4::identity();
// ... use it for internal math, write results as f32 to output buffer
```

### Serialization

```rust
use serde_json;
async fn config_handler(&mut self, ..., p: Pmt) -> Result<Pmt> {
    if let Pmt::String(json) = p {
        let cfg: MyConfig = serde_json::from_str(&json)?;
        self.apply_config(cfg);
    }
    Ok(Pmt::Ok)
}
```

### CRC / Checksums

```rust
// wlan_mac plugin uses crc32fast internally
use crc32fast::Hasher;
let mut h = Hasher::new();
h.update(&frame_bytes);
let checksum = h.finalize();
```

### I/O and Networking

```rust
use tokio::net::UdpSocket;
// Open socket in init(), read/write in work()
```

### Regex, Logging, Random Numbers

Any utility crate whose types don't appear in the block's port interfaces.

## Unsafe: Types Cross the Boundary

These patterns are **not safe** without extra care:

### Stream Buffer Types

The `D` in `Reader<D>` / `Writer<D>` must be the same concrete type in both
the producing and consuming block. If two plugins define their own
`MyComplex32` type independently, they'll have different `TypeId`s even if the
struct layout is identical. The connection will fail at `connect_dyn`.

**Solution**: Use types already in `libfuturesdr.so`:
- Primitives: `u8`, `u16`, `u32`, `u64`, `f32`, `f64`
- `num_complex::Complex32`
- `Vec<u8>` (via `Pmt::Blob`)

### Pmt::Any

```rust
// Plugin A sends
mio.post("out", Pmt::Any(Box::new(MyStruct { ... }))).await?;

// Plugin B receives
if let Pmt::Any(ref any) = p {
    let val = any.downcast_ref::<MyStruct>()?; // FAILS: different TypeId
}
```

`Pmt::Any` relies on `TypeId` for downcasting. If `MyStruct` is defined in
plugin A's crate, plugin B has a different `TypeId` for it — even if the
definition is textually identical.

**Solution**: Define the shared type in `futuresdr` itself (or a shared crate
that both plugins depend on via `libfuturesdr.so`), or use a standard `Pmt`
variant instead of `Pmt::Any`.

### Tag::NamedAny

Same issue as `Pmt::Any` — the `Box<dyn TagAny>` uses `Any::downcast`, which
requires matching `TypeId`s.

## When to Add Dependencies to FutureSDR

You need to add a dependency to FutureSDR (so it's part of `libfuturesdr.so`)
only when:

1. A new sample type (the `D` in `Reader<D>`) is needed that isn't already
   available (e.g., a custom `Complex64` type).
2. A new type needs to flow through `Pmt::Any` between plugins from different
   crates.
3. A new type needs to flow through `Tag::NamedAny` between plugins.

In all other cases, just add the dependency to your plugin's `Cargo.toml`.

## Static vs Dynamic Linking of External Crates

External crates in plugins are statically linked into the plugin `.so`. They
do **not** need to be in `libfuturesdr.so`. Each plugin can have its own
version of `serde`, `crc32fast`, etc. without conflict, as long as those
types stay internal.

```
┌─────────────────────────┐     ┌─────────────────────────┐
│  wlan_mac_plugin.so     │     │  zigbee_mac_plugin.so   │
│  ┌───────────────────┐  │     │  ┌───────────────────┐  │
│  │ crc32fast (static) │  │     │  │ crc32fast (static) │  │
│  └───────────────────┘  │     │  └───────────────────┘  │
│  ┌───────────────────┐  │     │  ┌───────────────────┐  │
│  │ block logic       │  │     │  │ block logic       │  │
│  └───────────────────┘  │     │  └───────────────────┘  │
└──────────┬──────────────┘     └──────────┬──────────────┘
           │                               │
           └───────────┬───────────────────┘
                       ▼
             ┌─────────────────┐
             │ libfuturesdr.so │  ← shared types only
             └─────────────────┘
```

## Summary

| Pattern | Safe? | Why |
|---------|-------|-----|
| `nalgebra` math inside `work()` | Yes | Types stay internal |
| `serde_json` for config parsing | Yes | JSON string crosses boundary, not serde types |
| `crc32fast` for checksums | Yes | Only `u32` result crosses boundary |
| Custom struct in `Pmt::Any` between plugins | No | `TypeId` mismatch |
| Custom struct as buffer sample type | No | `TypeId` mismatch in `connect_dyn` |
| `f32` / `Complex32` as buffer type | Yes | Defined in `libfuturesdr.so` |
| Standard `Pmt` variants (`Pmt::Blob`, etc.) | Yes | Enum layout shared |
