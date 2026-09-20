# FutureSDR plugins

Blocks compiled as shared libraries, loaded at run time, and flowgraphs
described in TOML that a controller links, replaces and retunes while the
others keep running.

This is an add-on: an independent Cargo workspace. The only change to
FutureSDR itself is `KernelInterface::IS_BLOCKING` (`src/runtime/
kernel_interface.rs`, used by `Flowgraph::add` and the `Block` derive), which
keeps plugins of non-blocking blocks from compiling the local-domain code.

| Crate | What it is |
|-------|------------|
| `api` | What plugins export: `export_plugin!`, block settings. |
| `rt` | `futuresdr-plugin-rt`: one shared copy of FutureSDR and the API, which hosts and plugins link, and the stream buffer they use. Feature `radio` (default) adds seify with the dummy driver, `soapy` adds SoapySDR. |
| `sdk` | `fsdr-plugin`: packs an SDK from a build of `rt`, builds and tests plugins against it. |
| `host` | `plugin_host`: registry, descriptions, controller. |
| `blocks/basic` | Generic blocks (`Head<f32>`, `Copy<u8>`, …), 74 block types. |
| `blocks/wlan` | 802.11a and 802.11ah (HaLow) receiver. |
| `blocks/zigbee` | 802.15.4 transceiver blocks. |
| `blocks/radio` | `SeifySource`, `SeifySink`. |

## Plugins

A plugin crate depends on nothing; the SDK provides `futuresdr_plugin_rt`:

```rust
extern crate futuresdr_plugin_rt as futuresdr;
use futuresdr::prelude::*;

export_plugin! {
    name: "mine",
    blocks: [
        // one block type per listed type: Delay<u8>, Delay<f32>
        { name: "Delay", types: [u8, f32], add: |s| blocks::Delay::<T>::new(s.get("n")?) },
        // a kernel from settings
        { name: "Mac", description: "…", add: |_s| Mac::new() },
        // blocks a helper adds to the flowgraph itself
        { name: "Modulator", build: |fg, _s| Ok(Added::untyped(block_on(modulator(fg))?)) },
    ]
}
```

```text
cargo build --release -p futuresdr-plugin-rt -p futuresdr-plugin-sdk
fsdr-plugin pack  --from target/release/libfuturesdr_plugin_rt.so --out sdk
fsdr-plugin build --sdk sdk path/to/plugin [--clippy] [--deny-warnings]
fsdr-plugin test  --sdk sdk path/to/plugin [-- test args]
```

A plugin only loads into a program linked with the same build of `rt`: the
SDK carries that build's metadata, and the registry refuses anything else.
Plugins are rebuilt when the SDK holds another build, even at the same path.
FFT plans come from `futuresdr::fft` (planning through `rustfft` would
compile all its algorithms into the plugin).

## Buffers

`rt/src/buffer` holds `circular_reuse`: FutureSDR's circular buffer, whose
ring is double-mapped into memory, with one change. When a flowgraph is
dropped its rings go to a pool (by item type and size, 64 MB by default,
`futuresdr::buffer::set_pool_limit`) rather than being unmapped, and the
next connection of that size takes one. Starting and stopping flowgraphs
then costs no mapping, no page faults on first use and no TLB flush on
release. A ring is kept only once its writer and all its readers are gone,
and a new reader starts where the writer is, so nothing of the previous
flowgraph shows.

All ports of a connection must use the same buffer. The plugin prelude's
`DefaultCpuReader`/`DefaultCpuWriter` are this buffer (`ReuseCpuReader`,
`ReuseCpuWriter`), so a plugin's own blocks use it without naming it.
Blocks that take their buffers as type parameters — FutureSDR's, and the
examples' blocks that `blocks/zigbee` compiles — default to FutureSDR's
buffer and are given this one: `blocks::Copy::<T, ReuseCpuReader<T>,
ReuseCpuWriter<T>>`. The host names it too, to read a block back:
`VectorSink<f32, ReuseCpuReader<f32>>`. A mismatch is refused when the
flowgraph starts ("dyn BufferReader has wrong type").

## Flowgraph descriptions

```toml
name = "halow"                     # optional; taps report it
plugins = ["libfsdr_blocks_wlan.so"]  # relative to this file
connections = """                  # FutureSDR connect! syntax
sync > long > eq > dec
"""

[blocks.sync]
type = "WlanSync<Ah>"              # a registered block type
threshold = 0.56                   # anything else is a setting

[inputs]                           # stream ports other flowgraphs feed
samples = { port = "sync.input", type = "c32" }
[outputs]                          # stream ports other flowgraphs read
# symbols = "eq.output"            # the type defaults to the block's parameter

[message_inputs]                   # message ports, linked the same way
# commands = "mac.tx"
[message_outputs]
frames = "dec.rx_frames"

[controls]                         # settings the flowgraphs this one feeds may ask for
# frequency = "src.freq"           # the message input that takes the value

[radio]                            # settings this one asks of those feeding it
frequency = 919.0e6
```

## Controller

```rust
let mut ctrl = Controller::new(registry);
ctrl.link("radio.samples", "halow.samples")?;   // one output, several inputs
ctrl.link("radio.samples", "zigbee.samples")?;
ctrl.park("zigbee.samples", Hold::Keep)?;       // gets nothing for now
let mut frames = ctrl.tap("halow.frames")?;     // messages for the application
ctrl.spawn("radio", Description::from_file("radio_head.toml")?)?;
ctrl.spawn("halow", Description::from_file("halow.toml")?)?;
ctrl.spawn("zigbee", Description::from_file("zigbee.toml")?)?;

ctrl.select("zigbee.samples", Hold::Discard)?;  // retunes the radio, then switches

let next = ctrl.prepare("halow", other)?;        // started, not linked yet
let old = ctrl.commit(next, Hold::Keep)?;        // microseconds
```

- **Replacing**: `replace` = `prepare` + `commit`. The new flowgraph starts
  before the old one lets go (make before break). `Hold::Keep` hands the
  input items the old one has not taken to the new one, `Hold::Discard`
  drops them. The old one finishes what it took in the background.
- **Links**: an output feeds any number of inputs, each with its own queue.
  `park`/`unpark` stop and resume a link; `select` makes one link of an
  output the only one that gets items, at once, so flowgraphs that all run
  take turns without being restarted. `unlink` removes a link.
- **Messages**: message outputs link to message inputs; a replaced
  flowgraph's last messages still arrive, a standby's once it is committed.
  `tap` gives an output's messages to the application, across
  replacements; `Tap::recv_from` also says which flowgraph posted each.
- **Controls**: a `[radio]` demand goes up the links to the nearest
  flowgraph whose `[controls]` offer it, and is set before the asking
  flowgraph's input switches (on commit or select), only if the value
  changes. While it changes, the links concerned get nothing, and the asking
  flowgraph starts on items that arrive after. A flowgraph started later, or
  replacing one that had settings, gets them first. Parked links ask for
  nothing; demands nobody offers are ignored.
- Every operation has an `_async` form. The blocking forms must not be
  called from the runtime's tasks; `Controller::run` runs async code as such
  a task, which also avoids waking a blocked thread for each operation.

## Receivers

`blocks/wlan` is one receiver for two standards, generic over `Standard`:

```text
WlanSync<S> > WlanSyncLong<S> > WlanEqualizer<S> > WlanDecoder<S>     S = A | Ah
```

- `A`: 802.11a/g, as `examples/wlan`.
- `Ah`: 802.11ah S1G 2 MHz at 4 MSps, as the `dyn` branch's HaLow v6 (which
  is `examples/wlan` moved to S1G).

`WlanSync` includes the delay line and moving averages that fed the
detector, `WlanEqualizer` the FFT. On the same recordings the plugin decodes
the same frames as the originals, whatever the chunk sizes
(`blocks/testdata/expected`): 17 + 1 frames of `examples/wlan`'s captures,
and the 112 frames v6 decodes from a one-second HaLow recording. On that
recording it runs at 76 MSps on one core; v6 ran at 49 MSps on about three.

`blocks/zigbee` compiles `examples/zigbee`'s blocks and adds the receiver's
demodulator, and a modulator that gives the example's samples (checked
against it) on this buffer.

## Examples

```text
cargo run --release --example swap_receivers   # replacing a receiver while a source runs
cargo run --release --example swap_bench       # replacement latency
cargo run --release --example ziglow_replay -- --mode all
```

`ziglow_replay` replays recorded 802.11ah and 802.15.4 frames, alternating,
and switches receivers after every frame, for inter-frame spacings from
4 ms down to 0 (`dyn`'s `ziglow_replay`). With 256-sample chunks and 4
workers, 200 frames per spacing:

| Mode | Switch (median) | PER at 0.5 ms | PER at 0 ms |
|------|-----------------|---------------|-------------|
| `select` (both run, links selected) | 3 µs | 0 % | 0 % |
| `standby` (prepared, committed) | 5 µs | 0 % | 7–10 % |
| `replace` (on demand, as `dyn`) | 0.13 ms | 1 % | 50–53 % |
| `both` (no switching) | – | 0 % | 0 % |

A receiver that is switched away from keeps the samples it already holds,
and a replaced one drains in the background, so both can still post frames
after a switch. Each frame is therefore matched to the transmission it
decodes — by the time it arrives, since decoding takes about 40 µs — and
counted only if its receiver was listening while that transmission was on
the air. What a radio would never have delivered, because the transmitter
had already moved to the other PHY, is reported as `impossible` and left
out; it stays at 0 except for a frame or two at the tightest spacings.

`--retune-us N` makes the replay a front end that takes N µs to change
frequency, which the receivers ask for.

## Tests

`./ci.sh` runs the pipeline CI runs (`.github/workflows/plugin.yml`):
formatting, lints, unit and integration tests, the plugin crates' tests
against their recordings, an SDK packed, moved and used end to end, size
budgets, the core change's tests, Miri, randomized tests at 20 times their cases, the controller tests under
load, and line coverage (at least 90 %). `./ci.sh all` runs every stage;
failing randomized tests print the `PLUGIN_TEST_SEED` that replays them.

## Against the `dyn` branch

| | this | `dyn` |
|-|------|-------|
| Shared library | 5.3 MB | 14.3 MB |
| One-block plugin | 114 KB | 150 KB |
| HaLow receive chain | 360 KB (802.11a too), 1 plugin | 4.0 MB, 11 plugins |
| ZigBee receive chain | 329 KB (transmit too) | 1 MB, 4 plugins |
| Replacement on demand | 0.09–0.15 ms | 0.20 ms |
| Prepared replacement | 0.01 ms | – |
| Bridge, `u8` / `f32` / `Complex32` | 8178 / 2042 / 1028 M items/s | 5607 / 1438 / 956 |
| Idle bridges | no CPU | about half a core (10 µs polling) |

FutureSDR's `Throttle` wakes on every item its reader takes: fed from a
fast source, it keeps the runtime busy (two cores at 4 MSps). Pace
replays in whole chunks, as `ziglow_replay`'s `Replay` block does.
