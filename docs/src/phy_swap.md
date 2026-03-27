# Dynamic PHY TX Swap: ZigBee to WLAN

This chapter walks through the `dyn_phy-swap` example — a complete
demonstration of runtime PHY switching from a ZigBee 802.15.4 TX chain to a
WLAN 802.11 TX chain, with every block loaded as a dynamic plugin.

## Overview

The example runs two sequential flowgraphs:

1. **Phase 1**: Build a ZigBee TX flowgraph from plugins, transmit N frames,
   terminate.
2. **Phase 2**: Build a WLAN TX flowgraph from plugins, transmit N frames,
   terminate.

No hardware is used — output goes to a `NullSink`. The point is to
demonstrate that the full TX pipeline for two different radio standards can be
assembled at runtime from separately compiled `.so` files.

## Plugin Loading

All plugins are loaded at startup, before any flowgraph is built:

```rust
let zigbee_mac = unsafe { LoadedPlugin::load("./libzigbee_mac_plugin.so") };
let zigbee_mod = unsafe { LoadedPlugin::load("./libzigbee_modulator_plugin.so") };
let zigbee_delay = unsafe { LoadedPlugin::load("./libzigbee_iq_delay_plugin.so") };
let null_sink = unsafe { LoadedPlugin::load("./libnull_sink_plugin.so") };

let wlan_mac = unsafe { LoadedPlugin::load("./libwlan_mac_plugin.so") };
let wlan_enc = unsafe { LoadedPlugin::load("./libwlan_encoder_plugin.so") };
let wlan_map = unsafe { LoadedPlugin::load("./libwlan_mapper_plugin.so") };
let fft = unsafe { LoadedPlugin::load("./libfft_complex_plugin.so") };
let wlan_pfx = unsafe { LoadedPlugin::load("./libwlan_prefix_plugin.so") };
```

Each `LoadedPlugin::load()` call:

1. Opens the `.so` with `dlopen`
2. Validates the ABI fingerprint against the runtime
3. Calls `create_block_factory()` to get the `BlockFactory`

## Phase 1: ZigBee TX

### ZigBee TX Chain

```
ZigbeeMac  →  ZigbeeModulator  →  ZigbeeIqDelay  →  NullSink
  (msg "tx")     (DSSS encode)      (I/Q offset)     (discard)
```

### Building the Flowgraph

```rust
let mut fg = Flowgraph::new();

let mac = fg.add_block_dyn(zigbee_mac.prepare(Box::new(())));
let modulator = fg.add_block_dyn(zigbee_mod.prepare(Box::new(())));
let delay = fg.add_block_dyn(zigbee_delay.prepare(Box::new(())));
let sink = fg.add_block_dyn(null_sink.prepare(Box::new("c32".to_string())));

fg.connect_message(mac, "tx", modulator, "tx")?;
fg.connect_dyn(modulator, "output", delay, "input")?;
fg.connect_dyn(delay, "output", sink, "input")?;
```

Note the mix of connection types:
- **Message connection**: `mac → modulator` (frame data as `Pmt::Blob`)
- **Stream connections**: `modulator → delay → sink` (Complex32 samples)

### ZigBee Modulation (DSSS)

The ZigBee modulator implements Direct Sequence Spread Spectrum:

1. Each byte is split into two nibbles (4 bits each)
2. Each nibble maps to a 32-chip spreading code (from a 16×16 lookup table)
3. Each chip is pulse-shaped with `[0.0, 0.707, 1.0, 0.707]`
4. Output: Complex32 I/Q samples

### Transmitting Frames

```rust
let rt = Runtime::new();
let (task, mut handle) = rt.start_sync(fg)?;

for _ in 0..n_frames {
    let frame = vec![0u8; 20];  // 20-byte payload
    handle.call(mac_id, "tx", Pmt::Blob(frame)).await?;
}

handle.terminate_and_wait().await?;
```

The `FlowgraphHandle` sends messages to the MAC block, which formats the
frame and forwards it to the modulator via the message connection.

## Phase 2: WLAN TX

### WLAN TX Chain

```
WlanMac  →(msg)→  WlanEncoder  →  WlanMapper  →  IFFT(64)  →  WlanPrefix  →  NullSink
                   (conv. code)    (OFDM map)    (freq→time)  (preamble)     (discard)
```

### Building the Flowgraph

```rust
let mut fg = Flowgraph::new();

let mac = fg.add_block_dyn(wlan_mac.prepare(Box::new((
    src_mac.to_string(),
    dst_mac.to_string(),
    bss_mac.to_string(),
))));
let encoder = fg.add_block_dyn(wlan_enc.prepare(Box::new("qpsk12".to_string())));
let mapper = fg.add_block_dyn(wlan_map.prepare(Box::new(())));
let ifft = fg.add_block_dyn(fft.prepare(Box::new((
    64u64,           // FFT size
    true,            // inverse
    true,            // fft_shift
    (1.0 / 52.0_f64.sqrt()),  // normalization
))));
let prefix = fg.add_block_dyn(wlan_pfx.prepare(Box::new((100usize, 100usize))));
let sink = fg.add_block_dyn(null_sink.prepare(Box::new("c32".to_string())));

fg.connect_message(mac, "tx", encoder, "tx")?;
fg.connect_dyn(encoder, "output", mapper, "input")?;
fg.connect_dyn(mapper, "output", ifft, "input")?;
fg.connect_dyn(ifft, "output", prefix, "input")?;
fg.connect_dyn(prefix, "output", sink, "input")?;
```

### WLAN Encoding Pipeline

**Encoder** (MCS: QPSK rate 1/2):
1. Scramble the payload with an LFSR
2. Convolutional encoding (rate 1/2, generators 0o133/0o171)
3. Puncturing (optional, based on MCS)
4. Interleaving (frequency-domain spreading for fading resistance)
5. Output: u8 symbols, one per OFDM subcarrier

**Mapper** (64-point OFDM):
- 48 data subcarriers + 4 pilot subcarriers
- DC subcarrier and guard bands zeroed
- Pilot polarity sequence for channel estimation
- Output: Complex32 frequency-domain symbols, 64 per OFDM symbol

**IFFT** (64-point inverse FFT):
- Transforms frequency domain to time domain
- Normalization factor: \\( 1/\sqrt{52} \\)
- FFT shift applied

**Prefix** (preamble and cyclic prefix):
- Short training sequence (160 samples, repeated)
- Long training sequence (160 samples)
- Cyclic prefix for each OFDM symbol
- Front/tail zero-padding (100 samples each)

### Transmitting WLAN Frames

```rust
let (task, mut handle) = rt.start_sync(fg)?;

for _ in 0..n_frames {
    let frame = vec![0u8; 100];  // 100-byte payload
    handle.call(mac_id, "tx", Pmt::Blob(frame)).await?;
}

handle.terminate_and_wait().await?;
```

## The Swap

The "swap" is simply terminating the first flowgraph and starting the second.
There is no running flowgraph modification — each phase is a complete
build-run-terminate cycle.

```
 Phase 1 (ZigBee)                   Phase 2 (WLAN)
┌──────────────────┐               ┌──────────────────┐
│ load plugins     │               │ build WLAN fg    │
│ build ZigBee fg  │               │ start runtime    │
│ start runtime    │──terminate──→│ send WLAN frames │
│ send ZB frames   │               │ terminate        │
│ terminate        │               └──────────────────┘
└──────────────────┘
```

This approach is simple and reliable:
- No need to hot-swap blocks in a running graph
- Each flowgraph is validated independently
- Plugin `.so` files stay loaded in memory across phases (the `LoadedPlugin`
  objects persist)

## RX Variant

The `main.rs` binary demonstrates the same concept for receive chains:

**Phase 1 (WLAN RX)**:
```
SeifySource → [power detection] → SyncShort → SyncLong → FFT(64) →
FrameEqualizer → WlanDecoder → BlobToUdp
```

**Phase 2 (ZigBee RX)**:
```
SeifySource → ZigbeeDemod → ClockRecoveryMm → ZigbeeDecoder → ZigbeeMac → NullSink
                                                                  ↓ msg
                                                              BlobToUdp
```

This variant uses a real SDR radio (via `SeifySource`) and outputs decoded
frames over UDP.

## Running the Example

```bash
# Build everything
./setup_plugins.sh

# Set library path
export LD_LIBRARY_PATH=./shared_libs/release:$LD_LIBRARY_PATH

# Run TX swap (no hardware needed)
./target/release/dyn_phy-swap-tx

# Run RX swap (requires SDR hardware)
./target/release/dyn_phy-swap --freq 2437000000 --gain 30
```

## Key Takeaways

1. **Plugin-based assembly**: Both PHY chains are built entirely from `.so`
   plugins — the binary contains no signal processing code.

2. **Standard patterns work**: `connect_dyn` and `connect_message` work
   identically to their static counterparts. The runtime doesn't distinguish
   between static and dynamic blocks.

3. **Type safety at connection time**: `connect_dyn` validates that buffer
   types match. A misconfigured connection (e.g., connecting a `u8` output to
   a `Complex32` input) fails immediately with a clear error.

4. **Config flexibility**: Each plugin's config type is chosen to match its
   constructor: `()` for no-config blocks, `String` for MCS selection,
   `(usize, usize)` for padding parameters, etc.

5. **Message passing across plugins**: The MAC-to-encoder message connection
   works across `.so` boundaries because `Pmt::Blob` is a standard variant
   defined in `libfuturesdr.so`.
