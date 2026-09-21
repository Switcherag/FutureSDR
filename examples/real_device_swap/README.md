# Real-device PHY swap

A receiver that changes PHY after every frame it decodes, between 802.11ah
(HaLow, 919 MHz) and 802.15.4 (ZigBee, 2.425 GHz), on a bladeRF 2.0 retuned
by quick tune. This is the `dyn` branch's `real_device_swap` example
(`ziglow_swap_quicktune`), running on the plugin controller of
`crates/plugin`.

```text
radio ──samples──▶ rx: HaLow ⇄ ZigBee   (Controller::replace after each frame)
```

The receivers are `flows/wlan_simple.toml` (the wlan plugin's `Ah`
receiver, fused blocks) and `flows/zigbee.toml` (examples/zigbee in the
zigbee plugin). `--halow wlan_granular.toml` uses the same HaLow receiver
built as `examples/wlan` builds its receiver (see below). Each asks for its
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

`--swap A,B` replays any two receivers in turn, including one replaced by
itself (`--swap zigbee.toml,zigbee.toml`); the IFS after B's frames is the
swept one unless `--ifs-z2h` sets it. `--retune-us 0` leaves out the front
end: the receivers' `[radio]` demands go nowhere, and only the software swap
is measured, as the `dyn` branch's software-IFS replay does.

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

## Simple or granular HaLow receiver

`flows/wlan_simple.toml` has four blocks: `WlanSync` includes the delay
line, the |x|² and x·conj(delayed) products, the two moving sums and their
ratio that feed `examples/wlan`'s short training field detector, and
`WlanEqualizer` includes the FFT. `flows/wlan_granular.toml` has them as
separate blocks, as `examples/wlan` connects them: 13 blocks and 15
connections. Both see the same frames (the wlan plugin's tests check this on
the 802.11a recordings and the HaLow frame, and on the one-second HaLow
recording, 112 frames).

With the replay, `--halow` takes several descriptions, run one after the
other, and the table splits the swap time by direction: a swap to HaLow is
the one that builds the HaLow receiver.

```text
cargo run --release -- --source replay --halow wlan_simple.toml,wlan_granular.toml
```

| | `wlan_simple.toml` | `wlan_granular.toml` |
|-|--------------------|----------------------|
| Blocks | 4 | 13 |
| One-second HaLow recording (release) | 76 MSps, 100 ms of CPU | 62 MSps, 260 ms of CPU |
| Replay: swap to HaLow (median) | 0.46–0.48 ms | 0.49–0.53 ms |
| Replay: swap to ZigBee (median) | 0.35 ms | 0.35 ms |

The fusion saves about 60 % of the CPU, and building and starting nine
more blocks costs about 0.04 ms per HaLow receiver. The PER curve of this
sweep does not move: the gap swept is the one before ZigBee frames, where
the ZigBee receiver is built (and PER near the knee varies from run to run
by several points).

## Software IFS

`figures/software_ifs.png`: PER and the median swap time against the IFS,
with no radio (`--retune-us 0`), 200 frames per spacing, for four swaps.
`figures/*.csv` are the runs and `figures/plot_software_ifs.py` draws them.

```text
cargo run --release -- --source replay --retune-us 0 --frames-per-step 200 \
    --ifs 2,1.5,1,0.8,0.6,0.5,0.4,0.35,0.3,0.25,0.2,0.15,0.1,0.05,0 \
    --swap zigbee.toml,zigbee.toml --csv figures/zz.csv
```

| Swap | Swap time (median) | PER |
|------|--------------------|-----|
| ZigBee → ZigBee | 0.05 ms | ≤ 1.5 % down to 0 |
| HaLow simple → simple | 0.15 ms | 0 % down to 0.35 ms, 50 % from 0.05 ms |
| HaLow granular → granular | 0.19 ms | ≤ 1 % down to 0.4 ms, 50 % from 0.1 ms |
| ZigBee ⇄ HaLow simple | 0.05 ms to ZigBee, 0.16 ms to HaLow | ≤ 2 % down to 0.25 ms, 50 % from 0.05 ms |

A swap starts when the receiver posts the frame, after it has decoded it,
so the time available is the IFS less the decoding delay, plus the silence
each recording keeps around its frame (about 0.07 ms for ZigBee, 0.05 ms for
HaLow). ZigBee's 0.05 ms swap fits in that silence alone. Near the knee, PER
moves by several points from run to run.
