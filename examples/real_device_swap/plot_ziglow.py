#!/usr/bin/env python3
"""Packet error rate against transmit IFS, per PHY, from a `ziglow_swap` capture.

The transmitter sends `--frames-per-step` frames at each IFS step, alternating
HaLow and Zigbee, and sweeps the IFS from `--ifs-start` down to `--ifs-stop`.
Half the frames at each step therefore go to each PHY, and PER is simply

    100 * (1 - received / sent)

per PHY per step. No adjacency, no streak analysis — just how many of the
frames that were sent came back.

The two PHYs are bucketed differently because they carry different stamps:

  * **Zigbee** frames carry the firmware stamp, so `run` *is* the step index
    and `wait_us` *is* that step's IFS. Nothing is inferred.
  * **HaLow** frames are RPG random payload with no stamp — only an 802.11
    sequence number. But the receiver alternates strictly Z, H, Z, H, so every
    HaLow frame sits between two Zigbee frames and inherits the step of the
    one before it. That holds exactly as long as the capture alternates, which
    the script checks and reports.

Usage:
    python3 plot_ziglow.py [csv] [--out plot.png] [--frames-per-step 100]

Defaults to ./ziglow_swap.csv and an interactive window.
"""

import argparse
import csv
import sys
from collections import Counter, defaultdict
from pathlib import Path

import matplotlib.pyplot as plt

# Zigbee red, HaLow blue — one hue per PHY, since PHY is the only thing
# distinguishing the two curves.
COLORS = {"Z": "#d13b30", "H": "#1f6fd0"}
NAMES = {"Z": "Zigbee 2.4 GHz", "H": "HaLow 919 MHz"}
CRITICAL = "#d03b3b"
MUTED = "#898781"
GRID = "#e1e0d9"
INK = "#0b0b0b"


def load(path):
    """Return `{phy: {step: received}}`, `{step: ifs_ms}`, and an alternation note."""
    with path.open(newline="") as handle:
        rows = [r for r in csv.DictReader(handle) if r.get("frame_event", "rx") == "rx"]
    if not rows:
        sys.exit(f"{path}: no received frames")

    # The Zigbee stamp is authoritative for both the step index and its IFS.
    ifs_ms = {}
    for r in rows:
        if r["phy"] == "Z" and r["run"] != "-1" and r["wait_us"] != "-1":
            ifs_ms[int(r["run"])] = int(r["wait_us"]) / 1000.0
    if not ifs_ms:
        sys.exit(f"{path}: no stamped Zigbee frames — cannot recover the sweep")

    received = defaultdict(Counter)
    current = None
    orphans = 0
    for r in rows:
        if r["phy"] == "Z" and r["run"] != "-1":
            current = int(r["run"])
            received["Z"][current] += 1
        elif r["phy"] == "H":
            # Inherit the step of the preceding Zigbee frame.
            if current is None:
                orphans += 1
                continue
            received["H"][current] += 1

    seq = "".join(r["phy"] for r in rows)
    breaks = seq.count("ZZ") + seq.count("HH")
    note = f"{len(rows)} frames, {breaks} alternation breaks"
    if orphans:
        note += f", {orphans} HaLow frames before the first stamp (dropped)"
    return received, ifs_ms, note


def summarise(received, ifs_ms, per_phy):
    """`{phy: [(ifs_ms, per_pct, rx, sent), ...]}`, sorted by descending IFS."""
    out = {}
    for phy, counts in received.items():
        series = []
        for step, ifs in sorted(ifs_ms.items()):
            rx = counts.get(step, 0)
            series.append((ifs, 100.0 * (1 - rx / per_phy), rx, per_phy))
        out[phy] = sorted(series, reverse=True)
    return out


def plot(data, per_phy, title, out):
    fig, ax = plt.subplots(figsize=(10.5, 6.4), layout="constrained")
    fig.patch.set_facecolor("#fcfcfb")
    ax.set_facecolor("#fcfcfb")

    for phy in sorted(data):
        series = data[phy]
        ax.plot(
            [d[0] for d in series],
            [d[1] for d in series],
            color=COLORS.get(phy, MUTED),
            linewidth=2,
            marker="o",
            markersize=5,
            markeredgecolor="#fcfcfb",
            markeredgewidth=0.6,
            label=NAMES.get(phy, phy),
        )

    floor = 100.0 / per_phy  # one frame in per_phy — the measurement resolution
    ax.axhline(floor, color=CRITICAL, linewidth=1, linestyle="--", alpha=0.7)
    ax.annotate(
        f"measurement floor — 1 frame in {per_phy}",
        xy=(max(d[0] for s in data.values() for d in s), floor),
        xytext=(4, 4), textcoords="offset points", color=CRITICAL, fontsize=9,
    )

    ax.set_xlabel("transmit IFS (ms) — sweep runs right to left", color=MUTED)
    ax.set_ylabel("packet error rate (%)", color=MUTED)
    ax.set_title(title, color=INK, loc="left", fontsize=13)
    ax.grid(True, color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    ax.tick_params(colors=MUTED)
    ax.set_ylim(-2, 102)
    for side, spine in ax.spines.items():
        spine.set_visible(side in ("left", "bottom"))
        spine.set_color("#c3c2b7")
    ax.invert_xaxis()  # sweep order: slowest first
    ax.legend(frameon=False, labelcolor=MUTED, loc="upper left")

    if out:
        fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="?", type=Path, default=Path("ziglow_swap.csv"))
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--frames-per-step", type=int, default=100,
                   help="frames sent at each IFS step, both PHYs together (default 100)")
    args = p.parse_args()

    if not args.csv.exists():
        sys.exit(f"missing {args.csv} — run `ziglow_swap` first")

    received, ifs_ms, note = load(args.csv)
    per_phy = args.frames_per_step // 2  # the sweep alternates, so half each
    data = summarise(received, ifs_ms, per_phy)

    print(f"== {args.csv.name}: {note}")
    print(f"   {len(ifs_ms)} IFS steps, {per_phy} frames per PHY per step")
    print(f"\n{'IFS ms':>7}" + "".join(f"{NAMES[p].split()[0]:>16}" for p in sorted(data)))
    for i, ifs in enumerate(sorted(ifs_ms.values(), reverse=True)):
        cells = "".join(
            f"{data[p][i][2]:>6}/{data[p][i][3]:<3} {data[p][i][1]:5.1f}%"
            for p in sorted(data)
        )
        print(f"{ifs:7.0f}{cells}")
    for phy in sorted(data):
        rx = sum(d[2] for d in data[phy])
        sent = sum(d[3] for d in data[phy])
        print(f"   {NAMES[phy]}: {rx}/{sent} → overall PER {100 * (1 - rx / sent):.1f}%")

    plot(data, per_phy, f"{args.csv.stem} — packet error rate vs transmit IFS", args.out)


if __name__ == "__main__":
    main()
