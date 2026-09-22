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

On a Raspberry Pi 5, see [RPI.md](RPI.md): installing libbladeRF, building,
CPU setup and the commands for every test.

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
(5) to the receivers' 4 MSps. A thread of its own reads it, 13 ms ahead of
the source block at most; samples are lost (the overflows) only when the
block is that late. As on the `dyn` branch, a swap does not wait for its
retune: the swap loop hands the frequency to a tuning thread and replaces
the receiver meanwhile. The source drops what was read before the retune,
and `--drop-after-retune-us` that much of what follows, for the samples
still in USB transfers from the previous channel.

It writes `real_device_swap.csv`, a row per frame (or timeout): the PHY, the
frame length, the time, how long the swap took, the last retune done (the
swap's own, or the one before if it is still running), and:

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

The recordings are cut with some silence around the frame. The replay cuts
them to the frame (where the power, averaged over 8 µs, is within 6 dB of
the frame's), so the IFS is from the end of a frame to the start of the next:

| Recording | Length | Frame | Silence before / after |
|-----------|--------|-------|------------------------|
| `halow_frame.cf32` | 730 µs | 683 µs (802.11ah PPDU: 680 µs) | 12 / 35 µs |
| `zigbee_frame.cf32` | 1410 µs | 1374 µs (43 bytes: 1376 µs) | 17 / 20 µs |

The `dyn` branch's generator put its gaps between whole recordings, so its
IFS was short of the real one by 32 to 52 µs (its metadata gives the ZigBee
frame as 1344 µs and 66 µs of silence; the frame is 1374 µs, and 37 µs of
silence). `--no-trim` replays the recordings whole, as it did.

`--swap A,B` replays any two receivers in turn, including one replaced by
itself (`--swap zigbee.toml,zigbee.toml`); the IFS after B's frames is the
swept one unless `--ifs-z2h` sets it. `--retune-us 0` leaves out the front
end: the receivers' `[radio]` demands go nowhere, and only the software swap
is measured, as the `dyn` branch's software-IFS replay does.

`--cpus auto` runs on one CPU of each of the fastest physical cores (as
many as `--workers`, 4 by default), a runtime thread pinned to each: on a
hybrid Intel CPU, performance cores; on a Raspberry Pi 5, CPUs 0 to 3.
`--cpus 8,10,0,2` names them. It prints each CPU's governor and maximum
frequency, which should be `performance` and the hardware's.

It writes `real_device_replay.csv`, a row per IFS. With 100 frames per IFS,
`--ifs-z2h 1`, a 300 µs retune and 4 runtime threads (before the
recordings were cut to the frame: add about 0.05 ms to the IFS):

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

`figures/software_ifs.png` and `figures/block_swap.png` are drawn from
`bench.sh`'s run on the laptop, `results/laptop-6ms` (six swaps, 0 to 6 ms
every 0.01 ms; `bench.png` and `summary.md` there), by
`figures/plot_software_ifs.py` and `figures/plot_block_swap.py`. Earlier
runs, below, had fewer spacings.

`figures/software_ifs.png`: PER and the median swap time against the IFS,
from the end of a frame to the start of the next (the recordings cut to the
frame), with no radio (`--retune-us 0`), 400 frames per spacing, for four
swaps.

```text
cargo run --release -- --source replay --cpus auto --keep-awake --retune-us 0 \
    --frames-per-step 400 --ifs 0,0.02,...,0.5,0.6,0.8,1 \
    --swap zigbee.toml,zigbee.toml --csv zz.csv    # or ./bench.sh, all six
```

| Swap | Swap time (median) | PER 50 % up to | PER 0–2 % from |
|------|--------------------|----------------|----------------|
| ZigBee → ZigBee | 0.035 ms | – | 0.02 ms |
| HaLow simple → simple | 0.15 ms | 0.10 ms | 0.20 ms |
| HaLow granular → granular | 0.19 ms | 0.16 ms | 0.24 ms |
| ZigBee ⇄ HaLow simple | 0.04 ms to ZigBee, 0.15 ms to HaLow | 0.08 ms | 0.16 ms |

A swap starts when the receiver posts the frame it decoded (0.04 ms after
the frame's end for ZigBee, 0.07 ms for HaLow, median), so a frame is lost
when the IFS is shorter than that plus the swap. Above the edges, every
frame is received but for one or two in 400.

### What the numbers need

With REPLAY_LOSSES set, the replay tells, for each lost frame, whether the
receiver was listening for its PHY from its start ("listening": none, in any
run: the receivers decode every frame they hear whole) or not ("late"), and
for late ones when the frame before was posted and how many samples waited
on the link. `--no-swap` keeps one receiver, for reference.

- **The link does not buffer.** When a swap begins, the link to the
  receiver holds no items (median 0, at most a few chunks when the receiver
  lags), and with `Hold::Discard` the new receiver starts on what arrives
  after the swap.
- **The replay runs on a thread of its own** (a blocking block, pinned
  away from the runtime's threads, pacing itself to the microsecond), as a
  radio's driver does. On the runtime's threads it waited for them whenever
  a swap kept them busy, 30 to 50 times per 400 frames by up to 2.8 ms, and
  then delivered in a burst, which closes the gap before the next frame.
- **Idle CPUs are slow to wake.** An idle core of this laptop sleeps in C3,
  which takes 1048 µs to leave, and its clock falls to 800 MHz: the
  `performance` governor only raises the ceiling. A runtime thread woken
  there posts its frame up to 1.9 ms late, once in 400 frames with no swap
  at all, and with swaps the next frame is lost (1–4 % at any IFS).
  `--keep-awake` keeps each CPU of `--cpus` busy with a thread at the lowest
  priority that yields at once to any other (it enters the kernel at every
  turn: with lazy preemption, a thread spinning in user space gives way
  only at the next tick). The worst posting delay falls from 1.9 to
  0.22 ms. It costs those cores' power; with root,
  `tuned-adm profile latency-performance` does the same (idle states up to
  C1, minimum clock 100 %).
- `--rt-priority N` gives the runtime's and the source's threads SCHED_FIFO,
  against other processes taking their CPUs; it needs an rtprio limit or
  CAP_SYS_NICE (not granted here, so not measured).

The HaLow receiver is the dyn branch's v6 (`examples/wlan` on S1G), and
includes the fix of dyn's commit 5d664f9a ("guard Signal state against full
output buffer"): the equalizer adds the `wifi_start` tag only with the first
data symbol it writes, so a full output cannot orphan it.

## Replacing one block

`flows/wlan_granular_viterbi.toml` and `flows/wlan_granular_hard.toml` are
`wlan_granular.toml` with its decoder `swappable`, one decoding by Viterbi
(`WlanDecoder<Ah>`), the other by the convolutional code's inverse, which
corrects nothing (`WlanHardDecoder<Ah>`). The decoder runs as a flowgraph
of its own, `rx/dec`, linked to the other twelve blocks, `rx`; replacing one
description with the other replaces the decoder only, and the equalizer's
output for the next frame, with its `wifi_start` tag, waits on the link for
the new one.

```text
cargo run --release -- --source replay --cpus auto --keep-awake --retune-us 0 \
    --swap wlan_granular_viterbi.toml,wlan_granular_hard.toml
```

`figures/block_swap.png` (`plot_block_swap.py`): the HaLow receiver replaced
whole after every frame (granular, simple and single-block) against the
granular one's decoder only, 400 frames per spacing.
The decoder alone takes 0.046 ms to replace against 0.187 ms, and no frame
is lost at any spacing (1 in 11,600 over the sweep), where replacing the
whole receiver loses half of them below 0.18 ms: the synchronizer and the
equalizer never stop, and a frame is only at risk if the old decoder took
its first bytes. Each description posted half of the frames (the replay
counts them by the name of the description that posted them).
