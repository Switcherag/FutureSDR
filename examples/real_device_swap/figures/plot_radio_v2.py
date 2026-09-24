#!/usr/bin/env python3
"""Plot a radio_bench run, placing frames by what they measure, not by counting.

The original plot_radio.py places a HaLow spacing by its POSITION: the n-th
group of frames is assumed to be the n-th spacing of the sweep. When a whole
spacing arrives empty, every later group shifts by one and the curve is wrong —
silently. In the 2026-09-24 run only 301 of 593 spacings produced frames, so the
curves were squeezed into 6.0-3.0 ms and everything below 3 ms was drawn as
100 % loss, which is an artifact and not a measurement.

Here a group is placed by ITS OWN arrival spacing instead. Within a group the
frames are IFS + airtime apart, so the shortest gap in the group measures the
IFS directly:

    ifs = min_gap - overhead

`overhead` (airtime plus the transmitter's per-frame cost) is constant across a
run, so it is estimated once, from the group with the largest min_gap, which is
the top of the sweep. The result is then snapped to the programmed grid.

ZigBee frames carry `ifs_us` in their payload, so they are placed by it when the
CSV has it (needs the light-frame parser); otherwise they fall back to gaps too.

Usage: plot_radio_v2.py <results-dir> [--ifs-start 6] [--ifs-step 0.01]
                        [--frames-per-step 10] [--ifs-floor 0.08] [--out FILE]
"""
import argparse
import csv
import math
import os
import statistics
import sys

from collections import Counter

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

PAUSE_FACTOR = 0.8      # a gap this much of the pause starts a new group


def rows_of(path):
    with open(path) as f:
        return [r for r in csv.DictReader(f) if r.get("event") == "rx"]


def groups_of(rows, pause_ms):
    """Cut a run's frames into groups at the transmitter's pauses."""
    groups, prev = [], None
    for r in rows:
        t = float(r["rx_t_ms"])
        if prev is None or t - prev > PAUSE_FACTOR * pause_ms:
            groups.append([])
        groups[-1].append(r)
        prev = t
    return groups


def min_gap(group):
    """The tightest arrival spacing in a group: IFS + overhead, in ms.

    The minimum, not the median: a frame lost inside a group doubles the gap
    around it, which pulls a median up but cannot pull the minimum down.
    """
    ts = [float(r["rx_t_ms"]) for r in group]
    gaps = [b - a for a, b in zip(ts, ts[1:])]
    return min(gaps) if gaps else math.nan


def grid(ifs_start, ifs_step, ifs_floor):
    n = round((ifs_start - ifs_floor) / ifs_step) + 1
    return [round(ifs_start - i * ifs_step, 6) for i in range(n)]


def place_by_schedule(groups, ifs_start, ifs_step, ifs_floor, frames_per_step,
                      pause_ms):
    """Give each group the spacing whose SCHEDULED time it arrived at.

    The transmitter's sweep is deterministic: spacing k sends
    `frames_per_step` frames `ifs_k + c` apart, then pauses `pause_ms`, so it
    starts at

        T(k) = k*pause + sum(frames_per_step * (ifs_i + c) for i < k)

    with `c` (airtime plus the transmitter's per-frame cost) the only unknown.
    Fitting `c` and the run's start offset to the observed group times places
    every group by WHEN it arrived, which survives spacings that went missing
    entirely — position-counting does not, and inverting a group's own arrival
    spacing is too noisy near the floor, where a lost frame doubles a gap.
    """
    g = grid(ifs_start, ifs_step, ifs_floor)

    def times(c):
        t, out = 0.0, []
        for ifs in g:
            out.append(t)
            t += frames_per_step * (ifs + c) + pause_ms
        return out

    starts = [float(gr[0]["rx_t_ms"]) for gr in groups if gr]
    if not starts:
        return {}, math.nan, 0

    # c is small next to the pause, so a coarse scan then a fine one is plenty.
    best = None
    for c in [x / 100.0 for x in range(0, 300)]:          # 0 … 3 ms
        T = times(c)
        t0 = starts[0] - T[0]
        err = 0.0
        for s_ in starts:
            k = min(range(len(T)), key=lambda i: abs(T[i] + t0 - s_))
            err += (T[k] + t0 - s_) ** 2
        if best is None or err < best[0]:
            best = (err, c, T, t0)
    _, c, T, t0 = best

    out, used = {}, set()
    for group in groups:
        if not group:
            continue
        s_ = float(group[0]["rx_t_ms"])
        k = min(range(len(T)), key=lambda i: abs(T[i] + t0 - s_))
        used.add(k)
        ifs = g[k]
        out[ifs] = (out.get(ifs, (0, 0))[0] + len(group), frames_per_step)
    return out, c, len(used)


def place_by_payload(rows, ifs_step):
    """ZigBee frames say which spacing they belong to."""
    out = {}
    for r in rows:
        try:
            ifs_us = int(r["ifs_us"])
        except (ValueError, KeyError):
            continue
        if ifs_us < 0:
            continue
        ifs = round(ifs_us / 1000.0, 6)
        out[ifs] = (out.get(ifs, (0, 0))[0] + 1, 0)
    return out


def sent_per_spacing(sizes):
    """How many frames a spacing holds, read off the data.

    The top of the sweep loses next to nothing, so the most common size among
    the first tenth of the spacings is what the transmitter actually delivers
    — which is not what it was configured to send: the rigs of 2026-09-24 are
    set to 10 frames a spacing and deliver 11 on the HaLow runs, one extra
    burst per spacing, so taking the figure from the firmware would report a
    9 % loss that is not there.
    """
    head = [n for n in sizes[: max(1, len(sizes) // 10)] if n > 1]
    return Counter(head).most_common(1)[0][0] if head else 0


def per_curve(placed, frames_per_step):
    xs = sorted(placed)
    ys = [100.0 * max(0.0, 1 - placed[x][0] / frames_per_step) for x in xs]
    return xs, ys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("results")
    ap.add_argument("--ifs-start", type=float, default=6.0)
    ap.add_argument("--ifs-step", type=float, default=0.01)
    ap.add_argument("--ifs-floor", type=float, default=0.08)
    ap.add_argument("--frames-per-step", type=int, default=0,
                    help="frames a spacing holds; 0 reads it off the top of the sweep")
    ap.add_argument("--pause-ms", type=float, default=500.0)
    ap.add_argument("--out", default=None)
    a = ap.parse_args()

    titles = {
        "zz": "ZigBee → ZigBee", "zc": "ZigBee ch15 ⇄ ch20",
        "sv": "HaLow simple: flowgraph", "si": "HaLow simple: decoder in place",
        "gv": "HaLow granular: flowgraph", "gi": "HaLow granular: decoder in place",
        "1v": "HaLow single: flowgraph", "1i": "HaLow single: in place",
        "sz": "HaLow ⇄ ZigBee",
        "zb": "CONTROL ZigBee, no swap", "hb": "CONTROL HaLow, no swap",
    }
    order = ["zz", "zb", "zc", "hb", "sv", "si", "gv", "gi", "1v", "1i", "sz"]

    runs, notes = {}, []
    for key in order:
        path = os.path.join(a.results, key + ".csv")
        if not os.path.exists(path):
            continue
        rows = rows_of(path)
        if not rows:
            continue
        groups = groups_of(rows, a.pause_ms)
        sent = a.frames_per_step or sent_per_spacing([len(g) for g in groups])
        by_payload = place_by_payload(rows, a.ifs_step)
        if len(by_payload) > 10:
            runs[key] = (by_payload, "payload", len(by_payload), sent)
            notes.append(f"{key}: {len(by_payload)} spacings from the frames' own IFS, "
                         f"{sent} frames each")
        else:
            placed, c, distinct = place_by_schedule(
                groups, a.ifs_start, a.ifs_step, a.ifs_floor,
                sent, a.pause_ms)
            runs[key] = (placed, "schedule", len(groups), sent)
            notes.append(f"{key}: {len(groups)} groups -> {distinct} distinct "
                         f"spacings, {sent} frames each "
                         f"(per-frame cost {c * 1000:.0f} µs)")

    if not runs:
        sys.exit("no runs with frames in " + a.results)

    fig, axes = plt.subplots(1, 2, figsize=(13, 5.5), sharey=True,
                             gridspec_kw={"width_ratios": [1.25, 1]})
    cmap = plt.get_cmap("tab10")

    ax = axes[0]
    for i, key in enumerate([k for k in order if k in runs]):
        placed, how, _, sent = runs[key]
        xs, ys = per_curve(placed, sent)
        if not xs:
            continue
        ax.plot(xs, ys, marker="o", ms=2.5, lw=1.1, alpha=0.85,
                color=cmap(i % 10), ls="-" if how == "payload" else "--",
                label=f"{titles.get(key, key)}")
    ax.set_xscale("log")
    ax.set_xlabel("inter-frame spacing (ms, log scale)")
    ax.set_ylabel("frames lost (%)")
    ax.set_title("Loss against the spacing the frames were sent at")
    ax.grid(alpha=0.3, which="both")
    ax.set_ylim(-3, 103)
    ax.legend(fontsize=7.5, ncol=2, loc="upper left", framealpha=0.9)

    ax = axes[1]
    for i, key in enumerate([k for k in order if k in runs]):
        placed, _, _, _ = runs[key]
        xs = sorted(placed)
        ys = [placed[x][0] for x in xs]
        if not xs:
            continue
        ax.plot(xs, ys, marker="o", ms=2.5, lw=1.1, alpha=0.85, color=cmap(i % 10))
    # What a spacing holds is read per run, so the line is only drawn where
    # the runs agree on it.
    sents = {r[3] for r in runs.values()}
    if len(sents) == 1:
        sent = sents.pop()
        ax.axhline(sent, color="0.35", lw=1, ls=":")
        ax.annotate(f"{sent} sent", (a.ifs_start, sent),
                    textcoords="offset points", xytext=(-4, 4), ha="right",
                    fontsize=8, color="0.35")
    else:
        ax.annotate(f"{min(sents)}-{max(sents)} sent per spacing", (0.02, 0.96),
                    xycoords="axes fraction", fontsize=8, color="0.35")
    ax.set_xscale("log")
    ax.set_xlabel("inter-frame spacing (ms, log scale)")
    ax.set_ylabel("frames received per spacing")
    ax.set_title("Frames received per spacing")
    ax.grid(alpha=0.3, which="both")

    fig.suptitle(os.path.basename(os.path.abspath(a.results)) +
                 " — solid: placed by the frame's own IFS, dashed: by arrival spacing",
                 fontsize=10)
    fig.tight_layout(rect=(0, 0.02, 1, 0.96))
    out = a.out or os.path.join(a.results, "radio_v2.png")
    fig.savefig(out, dpi=150)
    print("wrote", out)
    for n in notes:
        print(" ", n)


if __name__ == "__main__":
    main()
