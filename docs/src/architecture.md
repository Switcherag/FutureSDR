# Architecture Overview

FutureSDR's architecture is a layered dataflow system. This chapter provides
the 30,000-foot view before we dive into each layer in subsequent chapters.

## Layered Architecture

```
┌─────────────────────────────────────────────────┐
│                  User Application                │
│    (builds flowgraph, sends messages, reads UI)  │
├─────────────────────────────────────────────────┤
│                    Runtime                       │
│  (scheduler, task spawning, control port/REST)   │
├─────────────────────────────────────────────────┤
│                   Flowgraph                      │
│  (block registry, connection topology, startup)  │
├─────────────────────────────────────────────────┤
│                     Blocks                       │
│  (WrappedKernel<K>, message dispatch, work loop) │
├─────────────────────────────────────────────────┤
│                    Kernels                       │
│  (user-defined work(), init(), deinit())         │
├─────────────────────────────────────────────────┤
│                    Buffers                        │
│  (circular, slab, in-place, GPU, FPGA)           │
├──────────────────────┬──────────────────────────┤
│    Plugin Loader     │      Plugin .so files     │
│  (dlopen, ABI check) │  (BlockFactory, kernel)   │
└──────────────────────┴──────────────────────────┘
```

## Core Abstractions

### Flowgraph

A `Flowgraph` is a directed graph where nodes are blocks and edges are either
**stream connections** (typed sample buffers) or **message connections**
(asynchronous PMT messages). The user constructs a flowgraph, adds blocks,
connects them, and hands it to the runtime.

### Block

A `Block` is the runtime's unit of execution. It wraps a user-defined `Kernel`
with infrastructure for message dispatch, port management, and lifecycle
control. Blocks communicate through two mechanisms:

- **Stream ports**: high-throughput, typed, zero-copy circular buffers
- **Message ports**: low-throughput, polymorphic, asynchronous messages

### Kernel

A `Kernel` is a user-defined struct that implements the actual signal
processing. The user writes `work()`, `init()`, and `deinit()` methods. The
`#[derive(Block)]` macro generates all the boilerplate (port management,
message routing, type metadata).

### Runtime

The `Runtime` owns the async executor (based on `smol`), manages flowgraph
lifecycles, and exposes a REST/WebSocket control port. It can run multiple
flowgraphs concurrently.

### Buffer

Buffers connect stream ports between blocks. The default buffer is a
double-mapped circular buffer (`vmcircbuffer`) that provides zero-copy,
contiguous slices even when wrapping around the end of the buffer. Alternative
buffer backends can be plugged in for GPUs (Vulkan, WGPU), FPGAs (Zynq DMA),
or in-place processing.

## Data Flow

```
           ┌─────────┐    stream     ┌─────────┐    stream     ┌─────────┐
           │  Source  │─────────────→│  Filter  │─────────────→│  Sink   │
           │         │  [Complex32]  │         │  [Complex32]  │         │
           └─────────┘              └─────────┘              └─────────┘
                │                        │
                │ message "tx"           │ message "ctrl"
                ↓                        ↓
           ┌─────────┐              ┌─────────┐
           │   MAC   │              │  Config  │
           └─────────┘              └─────────┘
```

Stream data flows in a push model: a block calls `self.output.produce(n)` to
make `n` samples available downstream. The downstream block sees them in
`self.input.slice()`. The buffer notifies the downstream block via
`BlockMessage::Notify` so the scheduler knows to call its `work()` method.

Messages flow asynchronously: `mio.post("port_name", Pmt::...)` sends a
message to all connected handlers. Message handlers are async methods that
return `Result<Pmt>`.

## Type System

FutureSDR uses Rust's type system extensively:

- **Stream ports are generic over sample type `D`**: `Writer<D>` / `Reader<D>`
  where `D: CpuSample` (= `Default + Clone + Debug + Send + Sync + 'static`).
  Common types: `u8`, `f32`, `Complex32`.

- **Message ports use `Pmt`** (Polymorphic Message Type): an enum covering
  common SDR data types plus `Pmt::Any(Box<dyn PmtAny>)` for arbitrary types.

- **Blocks are generic over their I/O types**: `MyBlock<I, O>` where
  `I: CpuBufferReader<Item = f32>`, `O: CpuBufferWriter<Item = Complex32>`.
  The `#[derive(Block)]` macro provides sensible defaults.

## Compilation Model

FutureSDR compiles as:

```toml
crate-type = ["dylib", "rlib"]
```

- **`rlib`**: standard Rust static library, used by regular (non-plugin) builds
- **`dylib`**: Rust dynamic library (`libfuturesdr.so`), used by plugins

When the `plugin` feature is enabled, `impl Block for Box<dyn Block>` is
generated, allowing trait objects to be used as blocks. This is the mechanism
that lets dynamically loaded plugins return `Box<dyn Block>` through the
factory interface.

All plugins and the binary link against the same `libfuturesdr.so` at runtime,
sharing type definitions, vtable layouts, and generic instantiations.
