#!/usr/bin/env python3
"""Plot PER against transmit IFS from a `halow_full_swap` sweep capture.

The sweep steps IFS from 10 ms down to 1 ms in -0.1 ms steps, sending exactly
`--per-step` frames at each one, so the frame id partitions the capture: step
index is `seq // per_step` and its IFS is `start - step * step_size`.

Two error rates on one axis, since both are percentages of the same sweep:

  * packet error rate — what fraction of the 1000 frames never decoded
  * adjacent packet error rate — of the 999 consecutive frame-id pairs in a
    step, the fraction where the pair was NOT received intact. Two frames in a
    row count as one pair, three as two. It separates "losing every other
    frame" from "losing frames in bursts" at the same PER, which is what
    matters for a receiver that swaps between frames.

The y axis is symlog: log above the 0.1% measurement floor (1 frame in 1000),
linear below it, so steps with zero errors stay visible and truthful.

Usage:
    python3 plot_per_sweep.py [csv] [--out plot.png] [--per-step 1000]

Defaults to ./halow_switchv2.csv and an interactive window.
"""

import argparse
import csv
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.lines import Line2D

SEQ_MODULO = 4096  # the 802.11 sequence-number field is 12 bits

# One (base, tint) pair per capture, assigned in command-line order. Hue
# carries the capture, lightness the metric: the successive-frame curve is a
# tint of its own capture's colour, so the two curves read as a pair without
# consulting the legend. Line style reinforces it rather than carrying it
# alone, which keeps the chart legible in greyscale and for CVD readers.
SERIES = [
    ("#1f6fd0", "#93bfe9"),  # blue
    ("#d13b30", "#f0a69f"),  # red
    ("#1baf7a", "#93dcc3"),  # aqua — only if a third capture shows up
]
CRITICAL = "#d03b3b"
MUTED = "#898781"
GRID = "#e1e0d9"
INK = "#0b0b0b"


def phy_values(path):
    """Distinct `phy` values in a ziglow-style capture, or `[]` if not one."""
    with path.open(newline="") as handle:
        reader = csv.DictReader(handle)
        if "phy" not in (reader.fieldnames or ()):
            return []
        seen = []
        for r in reader:
            if r.get("frame_event") == "rx" and r["phy"] not in seen and r["phy"] != "?":
                seen.append(r["phy"])
    return seen


def load_steps(path, per_step, ifs_start, ifs_step, phy=None):
    """Return `[(ifs_ms, [received frame ids]), ...]`, one entry per IFS step.

    Two capture schemas are supported:

    * **stamped** (`zigbee_swap.csv`): the transmitter's firmware stamps every
      frame with `run` (the IFS step), `step` (frame counter within it) and
      `wait_us` (the IFS itself). Nothing is inferred.
    * **sequence-only** (`halow_switchv2.csv`): only the 802.11 sequence number
      is available, so steps are cut at every `per_step` frame ids and the IFS
      is reconstructed from `--ifs-start` / `--ifs-step`. Relies on the sweep
      running exactly as configured.
    """
    with path.open(newline="") as handle:
        reader = csv.DictReader(handle)
        fields = set(reader.fieldnames or ())
        rows = [r for r in reader if r.get("frame_event", "rx") == "rx"]
    if phy is not None:
        rows = [r for r in rows if r.get("phy") == phy]
    if not rows:
        raise ValueError("no received frames — was the capture truncated by a rerun?")

    if {"seq", "ifs_us"} <= fields and any(r["ifs_us"] != "-1" for r in rows):
        # zigbee_swap_short: every frame carries its own IFS, so the steps are
        # the distinct ifs_us values — no boundary inference. The sequence
        # number is only 8 bits and wraps ~4x within a 1000-frame step, so it
        # is unwrapped per step before being used as a frame id.
        stamped = [r for r in rows if r["ifs_us"] != "-1" and r["seq"] != "-1"]
        if not stamped:
            raise ValueError("has seq/ifs_us columns but no frame carries a stamp")
        by_ifs = defaultdict(list)
        for r in stamped:
            by_ifs[int(r["ifs_us"]) / 1000.0].append(int(r["seq"]))
        out = []
        for ifs, seqs in by_ifs.items():
            ids, offset = [], 0
            for i, s in enumerate(seqs):
                if i and s + offset < ids[-1] - 128:
                    offset += 256
                ids.append(s + offset)
            out.append((ifs, ids))
        return sorted(out)

    stamped = ([r for r in rows if r["step"] != "-1" and r["wait_us"] != "-1"]
               if {"run", "step", "wait_us"} <= fields else [])
    if stamped:
        by_run = {}
        for r in stamped:
            run = int(r["run"])
            entry = by_run.setdefault(run, [int(r["wait_us"]) / 1000.0, []])
            entry[1].append(int(r["step"]))
        return [(ifs, ids) for _, (ifs, ids) in sorted(by_run.items())]

    if "seq" not in fields:
        raise ValueError("needs either run/step/wait_us or a seq column")
    rows = [r for r in rows if r["seq"] != "-1"]
    if not rows:
        raise ValueError("no frame carries a usable sequence number")

    # Unwrap the 12-bit field so frame ids are monotonic across the whole sweep.
    unwrapped, offset = [], 0
    for i, r in enumerate(rows):
        s = int(r["seq"])
        if i and s + offset < unwrapped[-1] - SEQ_MODULO // 2:
            offset += SEQ_MODULO
        unwrapped.append(s + offset)

    base = unwrapped[0] - (unwrapped[0] % per_step)
    steps = defaultdict(list)
    for u in unwrapped:
        steps[(u - base) // per_step].append(u)
    return [(ifs_start + idx * ifs_step, steps[idx]) for idx in sorted(steps)]


def summarise(steps, per_step):
    """Per step: (ifs_ms, per_pct, adjacent_per_pct, received)."""
    out = []
    n_pairs = per_step - 1
    for ifs, ids in steps:
        got = sorted(set(ids))
        pairs = sum(1 for a, b in zip(got, got[1:]) if b - a == 1)
        out.append((
            ifs,
            100.0 * (per_step - len(got)) / per_step,
            100.0 * (n_pairs - pairs) / n_pairs,
            len(got),
        ))
    return sorted(out, reverse=True)


def plot(captures, per_step, title, out):
    """`captures` is `[(label, [(ifs, per, aper, rx), ...]), ...]`.

    Hue carries the capture, lightness the metric — so overlaying several
    sweeps stays readable and each pair of curves belongs together on sight.
    """
    floor = 100.0 / per_step  # one frame in per_step — the measurement resolution

    fig, ax = plt.subplots(figsize=(11.5, 6.8), layout="constrained")
    fig.patch.set_facecolor("#fcfcfb")
    ax.set_facecolor("#fcfcfb")

    # Log above the measurement floor, linear below, so the many zero-error
    # steps stay on the plot instead of being silently dropped by a log axis.
    ax.set_yscale("symlog", linthresh=floor, linscale=0.4)

    solo = len(captures) == 1
    for slot, (label, data) in enumerate(captures):
        base, tint = SERIES[slot % len(SERIES)]
        ifs = [d[0] for d in data]
        # Successive-frame curve first, so the packet-error curve sits on top
        # where the two converge.
        ax.plot(ifs, [d[2] for d in data], color=tint, linewidth=1.8,
                linestyle=(0, (5, 2)), marker="s", markersize=3.4,
                markeredgecolor="#fcfcfb", markeredgewidth=0.5)
        ax.plot(ifs, [d[1] for d in data], color=base, linewidth=2,
                marker="o", markersize=4, markeredgecolor="#fcfcfb",
                markeredgewidth=0.5, label=label)

    ax.axhline(floor, color=CRITICAL, linewidth=1, linestyle="--", alpha=0.7)
    ax.annotate(f"measurement floor — 1 frame in {per_step}",
                xy=(captures[0][1][0][0], floor), xytext=(4, 4),
                textcoords="offset points", color=CRITICAL, fontsize=9)

    ax.set_ylabel("error rate (%)", color=MUTED)
    ax.set_xlabel("transmit IFS (ms) — sweep runs right to left", color=MUTED)
    ax.set_title(title, color=INK, loc="left", fontsize=13)
    ax.grid(True, which="both", color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    ax.tick_params(colors=MUTED)
    for side, spine in ax.spines.items():
        spine.set_visible(side in ("left", "bottom"))
        spine.set_color("#c3c2b7")
    ax.invert_xaxis()  # sweep order: 10 ms first, 1 ms last

    # Shown in the first capture's own hues, so the legend demonstrates the
    # base/tint pairing rather than describing it in neutral grey.
    key_base, key_tint = SERIES[0]
    style_key = [
        Line2D([], [], color=key_base, linewidth=2, marker="o", markersize=4,
               label="packet error rate"),
        Line2D([], [], color=key_tint, linewidth=1.8, linestyle=(0, (5, 2)),
               marker="s", markersize=3.4, label="successive-frame error rate"),
    ]
    if solo:
        ax.legend(handles=style_key, frameon=False, labelcolor=MUTED, loc="upper left")
    else:
        first = ax.legend(frameon=False, labelcolor=MUTED, loc="upper left")
        ax.add_artist(first)
        ax.legend(handles=style_key, frameon=False, labelcolor=MUTED,
                  loc="upper left", bbox_to_anchor=(0.0, 0.84))

    if out:
        fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="*", type=Path,
                   help="one or more sweep CSVs; several are overlaid")
    p.add_argument("--labels", help="comma-separated legend labels, one per CSV")
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--per-step", type=int, default=1000,
                   help="frames the sweep sends at each IFS step (default 1000)")
    p.add_argument("--ifs-start", type=float, default=10.0, help="IFS of step 0, ms")
    p.add_argument("--ifs-step", type=float, default=-0.1, help="IFS delta per step, ms")
    args = p.parse_args()

    paths = args.csv or [Path("halow_switchv2.csv")]
    for path in paths:
        if not path.exists():
            sys.exit(f"missing {path}")
    labels = args.labels.split(",") if args.labels else [p.stem for p in paths]
    if len(labels) != len(paths):
        sys.exit(f"{len(labels)} labels for {len(paths)} files")

    captures = []
    for path, label in zip(paths, labels):
        # A capture with a `phy` column holds several PHYs interleaved; each
        # becomes its own overlaid series, since they sweep independently.
        for phy in (phy_values(path) or [None]):
            tag = f"{label} [{phy}]" if phy else label
            try:
                steps = load_steps(path, args.per_step, args.ifs_start,
                                   args.ifs_step, phy=phy)
            except ValueError as e:
                print(f"skipping {tag}: {e}", file=sys.stderr)
                continue
            data = summarise(steps, args.per_step)
            captures.append((tag, data))

            total_rx = sum(d[3] for d in data)
            total_tx = len(data) * args.per_step
            print(f"== {tag}: {len(data)} steps, {total_tx} sent, {total_rx} decoded "
                  f"→ overall PER {100 * (1 - total_rx / total_tx):.2f}%")
            print(f"{'IFS ms':>7} {'PER %':>8} {'adjPER %':>9} {'rx':>6}")
            for ifs, per, aper, rx in data[::10]:
                print(f"{ifs:7.1f} {per:8.2f} {aper:9.2f} {rx:6d}")

    if not captures:
        sys.exit("no usable captures")

    title = (f"{captures[0][0]} — packet and adjacent-packet error rate vs transmit IFS"
             if len(captures) == 1
             else "packet and adjacent-packet error rate vs transmit IFS")
    plot(captures, args.per_step, title, args.out)


if __name__ == "__main__":
    main()
