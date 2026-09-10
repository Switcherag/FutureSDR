# Replay bench — PER vs inter-frame spacing, without a radio

Recreates the packet-error-rate curve of `../../plot/per_soft.svg` from two
recorded reference frames instead of from a live over-the-air sweep. The
stimulus is a file, so it is byte-identical between runs: a change in PER
between two runs is a change in the receiver, not in the channel.

## The pipeline

    record_iq            ../halow_raw.cf32     1 s of baseband per standard
       │                 ../zigbee_raw.cf32
       │  (cropped by hand to one clean frame)
       ▼
    ../halow_frame.cf32   2920 samples,  730 µs   802.11ah
    ../zigbee_frame.cf32  5640 samples, 1410 µs   802.15.4
       │
       ▼  gen_iq.py --ifs X
    iq/ifs_X.cf32        [pad] H [X] Z [X] H … 1000 frames, strictly alternating
       │
       ▼  ziglow_replay --iq …
    csv/ifs_X.csv        one row per decoded frame (ziglow schema)
       │
       ▼  run_sweep.py
    results/per_replay.csv   ifs_ms, sent/received/PER per PHY, mean swap time

## Running it

    cd examples/real_device_swap
    cargo build --release -p real-device-swap-example --bin ziglow_replay
    python3 recording/bench/run_sweep.py          # 10.0 → 0.1 ms, 100 steps

About 11 minutes: the replay is paced on the wall clock, which is the point —
replaying as fast as the file can be read would make every swap free and every
PER zero. Runs are serial for the same reason; two at once compete for CPU and
each makes the other look worse at swapping.

The IQ file for each step is deleted once its step is done. Kept (`--keep`),
the full sweep is about 19.6 GiB; at any one moment it needs one file.

`--resume` skips steps already present in the results CSV.

## How PER is counted

Both reference frames are single recordings replayed verbatim, so every copy
carries the same firmware stamp and the same sequence number — no field inside
a frame can tell two transmissions apart. PER is therefore counted against the
generator's manifest, which knows exactly what it emitted:

    PER_phy = 1 − received_phy / sent_phy

A rate below zero means the receiver decoded more frames than were sent, i.e.
it is double-counting; that is reported rather than clamped.

## What the replay does and does not measure

`ziglow_replay` runs the same per-frame swap as `ziglow_swap`, between
`flows/zigbee_rxA.toml` and `flows/halowv6A.toml`, but there is no radio: the
controller's frequency and gain setters are bound to no-ops. Both flows still
declare `[radio]` sections and the controller still calls them; nothing acts.
So the cost being measured is the *software* swap — tear one flowgraph down,
stand the other up — and whether it finished before the next frame arrived.

The feeder keeps advancing the file across a swap even though the buffer is
gated off and cleared, exactly as a real radio keeps streaming into a receiver
that has stopped listening.

Watch for the overrun warning. A non-zero count means the flowgraph fell
behind real time, and that point's PER is not attributable to the swap alone;
`run_sweep.py` records it in the `overruns` column.

## Adding the curve to the figure

`plot_per_compare.py --overlay` draws a pre-aggregated PER curve alongside the
captures, and `--overlay-pool` collapses its two PHYs into one line by pooling
the counts. The baselines are the **v6** captures, matching the `halowv6A.toml`
the replay itself runs. `zigbee_swap.csv` is deleted in the working tree;
restore it from git first:

    cd examples/real_device_swap
    mkdir -p /tmp/softv6
    git show HEAD:examples/real_device_swap/zigbee_swap.csv > /tmp/softv6/zigbee_swap.csv
    cp halow_swapv6mcs0.csv halow_swapv6mcs6.csv /tmp/softv6/

    cd plot
    python3 plot_per_compare.py \
        --select halow_swapv6mcs0 halow_swapv6mcs6 zigbee_swap \
        --csv-dir /tmp/softv6 --paper --no-windows \
        --label 'halow_swapv6mcs0=802.11ah, MCS0' \
        --label 'halow_swapv6mcs6=802.11ah, MCS6' \
        --label 'zigbee_swap=802.15.4' \
        --overlay 'replay, no radio=../recording/bench/results/per_replay.csv' \
        --overlay-pool \
        --out per_soft.svg

Pooling is asked for explicitly because the two PHYs are *almost* always
identical — strict alternation loses them together — but not exactly: at three
IFS steps they differ by a single frame, which is enough to defeat an
equality test.

## Gaps are silent

`gen_iq.py` fills the inter-frame gaps with exact zeros by default. That is
what was asked for, but `halowv6A.toml`'s `div_mag` computes
`|MA96(x·conj(x_d))| / MA128(|x|²)`, which is 0/0 on a silent gap. If the
802.11ah trace ever collapses, regenerate with `--noise-dbfs -55` — roughly
the measured noise floor of the raw captures — and the gaps carry a realistic
floor instead.
