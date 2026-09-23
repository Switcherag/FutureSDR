# Note for the agent running the radio bench series

Written 2026-09-23 by the previous session, on the Pi (`framboise`, 131.254.100.44).
Adam asked for this series to be run automatically: you press Enter, he/you load the
transmitter firmware, the sweep runs, you press Enter, next run.

**Read section 3 before launching anything.** The flows, the bench keys and the plugin
work are done; what is left is one question about the ZigBee frames that decides whether
three of the nine runs can be analysed at all.

---

## 1. What the series is

Nine runs of `examples/real_device_swap/radio_bench.sh`, receiver replaced after every
received frame, PER against the programmed inter-frame spacing (IFS), bladeRF on the Pi,
transmitters (Seeed XIAO boards) on Adam's laptop.

Sweep: **from 6 ms down to 0.08 ms, step 10 µs** (593 spacings: `(6000 - 80) / 10 + 1`),
**1000 frames per spacing** (`FRAMES_PER_STEP=1000`, section 3.3). About 50 min per run,
~7.5 h for the nine.

All nine are wired up in `radio_bench.sh` as `ONLY="zz zc sv si gv gi 1v 1i sz"`, which is
its default now, in the order that changes the transmitter least:

| Key | Run | Swap kind | Transmitter |
|-----|-----|-----------|-------------|
| `zz` | ZigBee → ZigBee | flowgraph | ZigBee, 2.425 GHz |
| `zc` | ZigBee ch15 ⇄ ch20 | flowgraph (+ retune) | ZigBee alternating ch15/ch20 |
| `sv` | HaLow simple (4 blocks), viterbi ⇄ hard | flowgraph | HaLow, 919 MHz |
| `si` | HaLow simple, viterbi ⇄ hard | **decoder in place** | HaLow |
| `gv` | HaLow granular (13 blocks), viterbi ⇄ hard | flowgraph | HaLow |
| `gi` | HaLow granular, viterbi ⇄ hard | **decoder in place** | HaLow |
| `1v` | HaLow single block, viterbi ⇄ hard | flowgraph | HaLow |
| `1i` | HaLow single block, viterbi ⇄ hard | **block in place** | HaLow |
| `sz` | HaLow simple ⇄ ZigBee | flowgraph (+ retune) | ZigBee and HaLow alternating |

All HaLow receivers use the **no-CRC** decoder (`invalid_frames = true`): the transmitter
sends minimum-size frames with no payload, so there is no FCS to check (section 3.4).

**Flowgraph change vs block change.** A plain description is replaced whole
(`Controller::replace_async`). A description with a top-level `swappable = ["dec"]` is
spawned as segments, one flowgraph per swappable block, and a replacement that changes
only those blocks replaces just them, the rest keeps running
(`replace_swappable`, `crates/plugin/host/src/segments.rs`). The example needs **no code
change** for this: `--swap A,B` goes through `replace_async` either way. Syntax, from
`crates/plugin/host/tests/controller.rs:857`:

```toml
name = "halow/viterbi"
connections = "sync > long > eq > dec"
swappable = ["dec"]
```

If a segment's item type cannot be inferred, the error tells you to write
`[swappable] dec = { input = "u8" }`.

---

## 2. State of the machines

**Pi (`framboise`, this machine), `~/FutureSDR`,** a clone of `Switcherag/FutureSDR`,
branch `dynv4`:

- At `055e2516` (the plugin, flows and bench work of section 3.1), **one commit ahead of
  origin and not pushed** when this was written; Adam's `833d73fe nocrc` is pulled in. Push
  it with the recipe at the end of this section, or ask him.
- `figures/block_swap.png` and `figures/software_ifs.png` are modified locally and
  uncommitted, from before 2026-09-22. Leave them alone.
- `results/radio-framboise-2026092*` are untracked and **not in git**. A re-sync that
  replaces the tree wipes them — it already cost a run's data once. Copy them off the Pi
  before any such operation.
- Governor is `ondemand` after every reboot. Set `performance` (`sudo -n` works) before a
  run, or the numbers are not comparable with the earlier ones.
- `plot_radio.py` needs `/home/adam/.venvs/futuresdr-bench/bin` on PATH (matplotlib).
- `tmux` is installed. Adam wants long runs in tmux so he can disconnect;
  `KillUserProcesses=no`, so tmux survives logout.
- Disk: 46 GB free. A 593-spacing run at 1000 frames/spacing writes roughly a 35 MB CSV.

**bladeRF 2.0 micro xA9, serial 1895d41b…:** firmware 2.4.0, FPGA 0.15.3. Every run logs

```
[WARNING @ .../bladerf2.c:360] Using legacy message size. Consider upgrading firmware >= v2.5.0 and fpga >= v0.16.0
```

This is **harmless** (2 KiB instead of 8 KiB host↔FPGA messages, under 1 % overhead) and
is not the cause of any loss. Do not upgrade in the middle of the series — it would make
the runs before and after incomparable. Images are already downloaded, if the directory
still exists:
`/tmp/claude-1000/-home/7669731e-4410-4474-8089-e292c82b4087/scratchpad/{bladeRF_fw_v2.6.0.img,hostedxA9-v0.16.0.rbf}`
(upgrade: `bladeRF-cli -L <rbf>`, then `bladeRF-cli -f <img>`, then a power cycle; both
must move together).

**Laptop** (`alakhdar@131.254.23.112`, same /17, sshd open): transmitter project at
`/home/alakhdar/Projets/XiaoRadio/continuous_tx`. The Pi has a key for it at
`~/.ssh/id_ed25519_laptop`, but **it was not authorized yet** — the public key still has
to be appended to the laptop's `~/.ssh/authorized_keys`. Until then the Pi cannot start or
flash anything on the laptop; ask Adam, or drive the transmitter from a session on the
laptop itself.

**git push from the Pi:** there is no git identity and no credential helper, but `gh` is
logged in as `Switcherag`. What works:

```sh
git -c user.name="Switcherag" -c user.email="44577339+Switcherag@users.noreply.github.com" commit …
git -c credential.helper='!gh auth git-credential' push origin dynv4
```

---

## 3. What was prepared, and what is still open

### 3.1 Flows and plugin: done (commit `055e2516`, local, not pushed when this was written)

The twelve flows the six HaLow runs need are in `flows/`:
`halow_{simple,granular,single}_{viterbi,hard}[_inplace].toml`. Each pair differs by its
decoding alone; the `_inplace` ones declare that block `swappable`, so a swap replaces it
and the rest of the receiver keeps running. All are S1G ch34 (919 MHz) and all post
invalid frames (section 3.4).

The wlan plugin was restructured for this: `Decoder` held both decodings and a `hard`
flag, so replacing a `WlanDecoder` by a `WlanHardDecoder` replaced a block that already
contained the code it was replaced with. It is now generic over how the convolutional code
is undone (`Deconvolve`): `ViterbiDecoder` or `InverseDecoder`, and a block carries one of
them only. The fused receiver takes the same parameter, which gives `WlanHardReceiver` —
so the single-block runs (`1v`, `1i`), which had no hard-decision receiver to swap to, now
work.

Smoke-tested on the replay (20 frames a spacing at 6 ms, no frames lost), median swap:

| Receiver | Flowgraph replaced | That block replaced in place |
|---|---|---|
| simple (4 blocks) | 0.42 ms | 0.13 ms |
| granular (13 blocks) | 0.54 ms | 0.17 ms |
| single block | 0.48 ms | 0.53 ms |

The single-block receiver *is* the block, so segmenting it only adds a pipe; that it does
not get faster is the result, not a mistake.

### 3.2 What a swap's time is actually spent on

Measured with `REPLAY_SWAPS=<n>` (which prints build / start / switch per swap), 200
frames a spacing at 1 ms on the replay:

| Pair | Blocks built | build | start | switch | Swap median |
|---|---|---|---|---|---|
| simple, flowgraph | 6 | 0.36 | 0.19 | 0.01 | 0.52 ms |
| simple, decoder in place | 1 | 0.05 | 0.12 | 0.01 | 0.16 ms |
| granular, flowgraph | 14 | 0.40 | 0.41 | 0.01 | 0.87 ms |
| granular, decoder in place | 1 | 0.04 | 0.12 | 0.01 | 0.23 ms |
| single, flowgraph | 3 | 0.37 | 0.13 | 0.01 | 0.44 ms |
| single, that block in place | 1 | 0.35 | 0.10 | 0.01 | 0.45 ms |
| ZigBee, flowgraph | 7 | 0.04 | 0.19 | 0.01 | 0.23 ms |

`start` scales with the number of blocks (about 0.03 ms each). `build` does not: the
ZigBee receiver builds seven blocks in 0.04 ms while the one-block HaLow receiver takes
0.37 ms. The difference is one constructor — `SyncLong::new` calls `Ah::ltf_taps`
(`crates/plugin/blocks/wlan/src/ah.rs`), which recomputes the matched filter as a 64 x 64
inverse DFT, with a `from_polar` (sine and cosine) per term, every time a receiver is
built. Caching those taps in a `OnceLock` (tried, then reverted) drops the simple
flowgraph swap from 0.52 to 0.27 ms and the single-block one from 0.44 to 0.19 ms.

So **the HaLow swap times in these runs are mostly a receiver-side constructor, not the
runtime's replacement machinery**, and that is what the 2026-09-22 radio run measured too
(HaLow medians 0.41–0.54 ms; `gd`, the decoder alone, 0.17 ms). Whether to cache the taps
before the series is Adam's call — see the open questions.

### 3.3 Frames per spacing: 1000 (settled)

Adam first wrote *"10000 points from 6ms to 0.08ms with padding of 10us"*, which did not
fit: 6 ms → 0.08 ms in 10 µs steps is **593 spacings**, not 10000. Asked, he answered
"do 1000 per run", which reads as **1000 frames per spacing**, the same as the previous
runs (they were 1000 per spacing over 61 spacings): `FRAMES_PER_STEP=1000`, 593 000 frames
per run. If he meant 1000 frames for a whole run, that would be under 2 frames per
spacing, which measures nothing — so if the transmitter turns out to be set up that way,
stop and ask rather than run it.

Cost, from the measured per-frame time (IFS + ~1.4 ms for ZigBee, IFS + ~2 ms for HaLow;
mean IFS over the sweep is 3.04 ms): about **50 min per run**, about **7.5 h** for the nine,
plus the HaLow pauses (593 × 0.5 s ≈ 5 min per HaLow run). CSV per run ~35 MB.

**The transmitter firmware and `FRAMES_PER_STEP` must agree**, as must the 6 ms start and
the 10 µs step — a mismatch here is exactly what broke the last analysis (section 5).

### 3.4 Why the CRC is off, and what it costs the analysis

Adam's transmitter is a fast symbol generator that sends **minimum-size frames with no
payload**, to keep the frames short. There is no FCS to check, so a CRC-checking receiver
would post nothing: every HaLow flow of the series sets `invalid_frames = true`. This is
not a diagnostic setting, it is what makes the runs possible.

What follows from it:

- PER counts the frames that failed to decode at all. It is not the same quantity as the
  2026-09-22 run, where frames had a payload and a checked FCS. Say so in the summary.
- **No payload means no 802.11 MAC header, so no sequence number** (`parse_seq` in
  `src/main.rs` wants 24 bytes), and the CSV's `seq` is then `-1`. `plot_radio.py` used to
  drop those rows, which would have left every HaLow curve empty; it now counts a
  spacing's frames as they arrive when they carry no sequence number, and by distinct
  sequence number when they do. Counting arrivals cannot tell a repeat from a new frame,
  so a duplicate would read as a received frame.
- **Check the ZigBee side before trusting `zz`, `zc` and `sz`.** Those runs are placed on
  the sweep by the multizig stamp, which lives in the ZigBee payload. If the ZigBee
  transmitter also sends payload-less frames now, the stamp is gone, `ifs_us` is `-1`, and
  those three runs cannot be placed at all — the analysis would need the same pause-cut
  treatment as the HaLow ones. Ask Adam, or look at one `zz` CSV: if `step`/`ifs_us` are
  `-1`, stop and say so.

---

## 4. Running it

`radio_bench.sh` is interactive by design: per run it prints what transmitter it needs,
waits for Enter, starts the receiver, and the receiver prints
`receiving; press Enter to stop`. Drive it from tmux:

```sh
cd ~/FutureSDR/examples/real_device_swap
sudo -n cpufreq-set -g performance 2>/dev/null || for c in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance | sudo -n tee "$c" >/dev/null; done
export PATH=/home/adam/.venvs/futuresdr-bench/bin:$HOME/.cargo/bin:$PATH
tmux new-session -d -s radio -c ~/FutureSDR/examples/real_device_swap \
  'FRAMES_PER_STEP=1000 IFS_START=6 IFS_STEP=0.01 PAUSE_MS=500 ./radio_bench.sh 2>&1 | tee ~/radio-series.log'
tmux capture-pane -pt radio | tail -20     # see where it is
tmux send-keys -t radio Enter              # answer a prompt
```

Per run, the loop is:

1. `capture-pane` until the prompt `Press Enter to start the receiver` is there.
2. `send-keys Enter`.
3. Wait for `receiving; press Enter to stop` — **not before**. Starting the transmitter
   earlier costs the first spacing and shifts every HaLow spacing after it.
4. Load that run's transmitter firmware on the laptop and let the sweep run.
5. The receiver prints a counter line every 5 s. The sweep is over when the frame counts
   stop rising (and the transmitter says it is done). Give it a few seconds of margin.
6. `send-keys Enter` to stop the receiver. The script goes to the next run.

Smoke-test the whole chain first with a short sweep (a handful of spacings, 20 frames
each) before committing to hours of runs, especially for the block-change runs (4, 6, 8):
the swapped block `dec` is the one that posts `frames`, and message outputs across a
segment boundary are handled by `segments.rs`, but this pairing has not been run on real
hardware yet. Check that frames keep arriving after the first swap.

Each run writes `<key>.csv` (a row per received frame), `<key>.log`, and the directory
gets `system.txt`, `radio.png` and `summary.md` at the end.

Sanity checks per run, in `summary.md` / the log:

- `radio overflows 0` and `quick-tune misses 0`. Non-zero overflows mean the Pi could not
  keep up: try `EXTRA="--sample-rate 4e6 --decim 1"` (4 MSps straight from the radio
  instead of 20 MSps decimated by 5).
- Received/sent in the right ballpark; PER near 0 at 6 ms.
- The analysis note about how HaLow frames were placed (see section 5).

---

## 5. Two traps that already cost a run — do not fall in them again

The 2026-09-22 run (`results/radio-framboise-20260922-1443`) produced a nonsense plot for
both reasons:

1. **The sweep the analysis assumes must be the sweep the transmitter runs.** The
   transmitters swept 61 spacings of 0.1 ms; the script was left at `IFS_STEP=0.01`, so
   all HaLow curves were crammed between 5.4 and 6 ms. ZigBee was unaffected (its frames
   carry their own IFS in the stamp), HaLow was not (placed by pause-cut order).
   `plot_radio.py` warns `N parts for M spacings` when they disagree — **treat that
   warning as an error**, except for the expected one below.
   With this series stopping at 0.08 ms rather than 0, expect exactly
   `593 parts for 601 spacings`: that one is normal, the sweep simply ends early. Any
   other count means the transmitter and the settings disagree.
2. **ZigBee frame numbering is per step, across channels.** In the ch15⇄ch20 run the
   transmitter numbers a step's frames 0…999 over both channels (ch15 even, ch20 odd).
   Counting per channel doubled the expected total and drew a flat 50 % PER. Fixed in
   `3ff2ad43` (already on origin) — do not reintroduce a per-channel count.

Re-plotting a finished directory is cheap and does not need the radio:

```sh
python3 figures/plot_radio.py results/<dir> --frames-per-step <N> --ifs-start 6 --ifs-step 0.01 --pause-ms 500
```

`--pause-ms 500` is the transmitter's pause between spacings, which is how HaLow-only runs
are cut into spacings; the measured pauses were ~528 ms. It must stay much longer than the
largest IFS.

---

## 6. Open questions for Adam

1. Whether the ZigBee transmitter still stamps its frames (section 3.4). This decides
   whether `zz`, `zc` and `sz` can be analysed as they are.
2. Whether the two decoders should also become two plugin libraries
   (`decoderViterbi.so`, `decoderHard.so`). Adam asked; today they are two block types in
   one `libfsdr_blocks_wlan.so`, which already means a swap replaces the code that runs.
   Splitting the libraries is blocked by the SDK (a plugin crate has no dependencies) and
   by type identity across libraries: the equalizer hands the decoder a `FrameParam` in a
   tag, and two libraries each compiling their own `FrameParam` cannot downcast each
   other's.
3. Whether to cache `Ah::ltf_taps` before running the series (section 3.2). It takes about
   0.3 ms off every swap that rebuilds a whole HaLow receiver, which is most of what those
   runs currently measure. Caching makes the runs measure the replacement machinery;
   leaving it makes them measure what rebuilding these receivers costs as written. Either
   is defensible, but it should be decided before the series, not between runs.
4. Authorizing the Pi's key on the laptop, if the transmitter is to be driven from the Pi.

Settled: 1000 frames per spacing (section 3.3); the flows, the nine bench keys and the
hard-decision single-block receiver (section 3.1); no-CRC everywhere (section 3.4).
