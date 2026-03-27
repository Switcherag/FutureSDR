# FutureSDR Dynamic Plugin Architecture

This book provides an in-depth technical reference for the FutureSDR framework
and its dynamic plugin system. It is written for computer engineers who want to
understand not just *how* to use the framework, but *why* it works the way it
does at the systems level.

## What is FutureSDR?

FutureSDR is an asynchronous Software-Defined Radio (SDR) runtime for
heterogeneous architectures. It provides a dataflow programming model where
signal processing is expressed as a directed graph of **blocks** connected by
**streams** (typed sample buffers) and **messages** (polymorphic control data).

Key properties:

- **Asynchronous**: Built on Rust's `async`/`await` with the `smol` executor.
  Blocks yield cooperatively, enabling efficient multiplexing of I/O-bound and
  compute-bound work on a small thread pool.
- **Extensible**: Custom buffer backends (GPU, FPGA, DMA) and custom schedulers
  can be plugged in without modifying the core runtime.
- **Portable**: Runs on Linux, Windows, macOS, Android, WebAssembly, and
  bare-metal embedded (via REST/WebSocket control).
- **Fast**: Zero-copy circular buffers using double-mapped virtual memory,
  SIMD-friendly contiguous slices, and minimal runtime overhead.

## What is the Dynamic Plugin System?

The plugin system allows FutureSDR blocks to be compiled as separate shared
libraries (`.so` files on Linux) and loaded at runtime via `dlopen`. This
enables:

1. **Hot-swappable PHY layers** — switch from one radio standard to another
   without restarting the application.
2. **A-posteriori extensibility** — build new blocks after the runtime is
   deployed, without access to the full source tree.
3. **Reduced compile times** — change one block, rebuild one `.so`, reload.
4. **Third-party plugins** — distribute blocks as binary artifacts.

## How This Book is Organized

**Part I** covers the FutureSDR framework internals: the runtime engine,
flowgraph construction, block lifecycle, buffer system, and message passing.
You need this foundation to understand how plugins integrate.

**Part II** covers the dynamic plugin system itself: the plugin ABI, how to
write plugins, the ABI fingerprint mechanism, external library rules, and the
SDK for a-posteriori compilation.

**Part III** is a complete case study: dynamically swapping from a ZigBee
802.15.4 TX chain to a WLAN 802.11 TX chain, with every block loaded as a
plugin at runtime.

## Prerequisites

This book assumes familiarity with:

- Rust (ownership, traits, generics, async/await, dynamic dispatch)
- Basic DSP concepts (samples, sample rates, modulation)
- Linux shared libraries (ELF, `dlopen`, symbol resolution, `LD_LIBRARY_PATH`)
- The concept of SDR flowgraphs (if you've used GNU Radio, you're set)
