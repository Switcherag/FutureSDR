# Real-device PHY swap

A receiver that changes PHY after every frame it decodes, between 802.11ah
(HaLow, 919 MHz) and 802.15.4 (ZigBee, 2.425 GHz), on a bladeRF 2.0 retuned
by quick tune. This is the `dyn` branch's `real_device_swap` example
(`ziglow_swap_quicktune`), running on the plugin controller of
`crates/plugin`.

```text
radio ──samples──▶ rx: HaLow ⇄ ZigBee   (Controller::replace after each frame)
```

The receivers are `flows/halow.toml` (the wlan plugin's `Ah` receiver) and
`flows/zigbee.toml` (examples/zigbee in the zigbee plugin). Each asks for its
channel in its `[radio]` section, and the controller sets it on the radio
before the new receiver gets samples. The plugins are built at start against
the shared library this program runs (in `target/plugins`), or loaded from
`--plugins DIR`.

## With a bladeRF

```text
cargo run --release -- --source bladerf --register-only   # quick-tune profiles
cargo run --release -- --source bladerf --duration 60
```

The radio is driven through libbladeRF (the `bladerf` crate, the bindings
the `dyn` branch used, pinned to the same revision), which must be installed;
`build.rs` finds it with pkg-config. At start, the radio is tuned to each
receiver's channel once and the RFIC state kept, so a retune is a
quick-tune recall (about 0.3 ms across bands, against 26.9 ms in FPGA tuning
mode). `--no-quick-tune` uses `set_frequency` instead, to compare.

The bladeRF runs at `--sample-rate` (20 MSps) and is decimated by `--decim`
(5) to the receivers' 4 MSps. `--drop-after-retune-us` drops that much of
the samples that follow a retune, for those still in USB transfers from the
previous channel.

It writes `real_device_swap.csv`, a row per frame (or timeout): the PHY, the
frame length, the time, how long the swap and its retune took, and:

- for ZigBee frames of the `dyn` branch's multizig transmitter, the stamp it
  puts after its source address: frame number, step, tag, the inter-frame
  spacing (IFS) the transmitter was set to, and its clock;
- for HaLow frames, the 802.11 sequence number.

Without the feature (`--no-default-features`), only the replay is built.

## With replayed frames

```text
cargo run --release -- --source replay
cargo run --release -- --source replay --ifs-h2z 0.5,0.3,0.2 --ifs-z2h 0.5 --retune-us 300
```

One recorded frame of each PHY is replayed, H Z H Z ..., at 4 MSps of wall
clock, in place of the radio. The IFS after each HaLow frame, the time the
receiver has to become a ZigBee receiver and retune from 919 MHz to
2.425 GHz, is swept by `--ifs-h2z` (`START:STOP:STEP` or a list, ms); the IFS
after each ZigBee frame is `--ifs-z2h`. The replayed front end takes
`--retune-us` to change channel and loses the samples meanwhile. Each frame
is matched to the transmission it decodes and counted only if the receiver
was listening while it was on the air.

It writes `real_device_replay.csv`, a row per IFS. With 100 frames per IFS,
`--ifs-z2h 1`, a 300 µs retune and 4 runtime threads:

| IFS after H | PER H | PER Z | swap (median) | of which retune |
|-------------|-------|-------|---------------|-----------------|
| 1.0–0.5 ms  | 0 %   | 0 %   | 0.44 ms       | 0.31 ms         |
| 0.4–0.3 ms  | 2 %   | 2 %   | 0.44 ms       | 0.31 ms         |
| 0.2 ms      | 14 %  | 14 %  | 0.44 ms       | 0.31 ms         |
| 0.1–0 ms    | 50 %  | 50 %  | 0.45 ms       | 0.31 ms         |

The swap needs about 0.45 ms, but the recordings keep a little silence
around each frame, so it only starts to fail below 0.4 ms. HaLow frames are
lost as often as ZigBee ones although their IFS does not change: the
receiver only changes on a frame, so after a missed ZigBee frame it is still
a ZigBee receiver when the next HaLow frame arrives.
