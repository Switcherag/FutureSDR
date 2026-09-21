#!/usr/bin/env python3
"""The five swaps of bench.sh: PER and swap time against the IFS.

    python3 plot_bench.py RESULTS_DIR

Reads RESULTS_DIR/{zz,ss,sz,gg,gd}.csv (those present) and writes
RESULTS_DIR/bench.png and RESULTS_DIR/summary.md.
"""
import csv
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# Categorical slots 1-5 in order (validated with the dataviz method's
# script); the dash of each line tells them apart too.
SERIES = [
    ("zz", "ZigBee → ZigBee", "#2a78d6", "-"),
    ("ss", "HaLow simple → simple", "#eb6834", "--"),
    ("sz", "HaLow simple ⇄ ZigBee", "#1baf7a", "-."),
    ("gg", "HaLow granular → granular", "#eda100", ":"),
    ("gd", "HaLow granular, decoder only (inverse ⇄ Viterbi)", "#e87ba4", (0, (5, 1, 1, 1))),
]
SURFACE, GRID, AXIS = "#fcfcfb", "#e1e0d9", "#c3c2b7"
INK, INK2, MUTED = "#0b0b0b", "#52514e", "#898781"
ZOOM = 0.6


def load(path):
    # The IFS from frame end to next frame start: with recordings replayed
    # whole (--no-trim), the gap plus the silence they keep; older runs
    # have only the gap.
    key = "ifs_true_ms" if "ifs_true_ms" in open(path).readline() else "ifs_ms"
    rows = sorted(csv.DictReader(open(path)), key=lambda r: float(r[key]))
    dropped = sum(int(r.get("source_dropped") or 0) + int(r.get("link_dropped") or 0)
                  for r in rows)
    return {
        "dropped": dropped,
        "ifs": [float(r[key]) for r in rows],
        "per": [100 * float(r["per"]) for r in rows],
        "swap": [float(r["swap_median_ms"]) for r in rows],
    }


def median(v):
    v = sorted(x for x in v if x == x)
    return v[len(v) // 2] if v else float("nan")


def edge(d, limit=1.0):
    """The smallest IFS from which PER stays at or below `limit` %."""
    at = None
    for ifs, per in zip(reversed(d["ifs"]), reversed(d["per"])):
        if per > limit:
            break
        at = ifs
    return at


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    data = [(key, label, color, dash, load(out / f"{key}.csv"))
            for key, label, color, dash in SERIES if (out / f"{key}.csv").exists()]
    if not data:
        sys.exit(f"no results in {out}")
    system = (out / "system.txt").read_text().splitlines()[0] if (out / "system.txt").exists() else ""

    fig, (ax_all, ax_zoom, ax_swap) = plt.subplots(
        3, 1, figsize=(13, 11), gridspec_kw={"height_ratios": [2, 2, 1.6]}
    )
    fig.patch.set_facecolor(SURFACE)
    for ax in (ax_all, ax_zoom, ax_swap):
        ax.set_facecolor(SURFACE)
        ax.grid(True, color=GRID, linewidth=0.8)
        ax.set_axisbelow(True)
        for side in ("top", "right"):
            ax.spines[side].set_visible(False)
        for side in ("left", "bottom"):
            ax.spines[side].set_color(AXIS)
        ax.tick_params(colors=INK2, labelsize=10)

    for key, label, color, dash, d in data:
        style = dict(color=color, linewidth=1.8, linestyle=dash, label=label)
        ax_all.plot(d["ifs"], d["per"], **style)
        zoom = [(i, p) for i, p in zip(d["ifs"], d["per"]) if i <= ZOOM]
        ax_zoom.plot([i for i, _ in zoom], [p for _, p in zoom], **style)
        ax_swap.plot(d["ifs"], d["swap"], **style)

    ax_all.set_title("Packet error rate, 0 to 4 ms", loc="left", color=INK2, fontsize=11)
    ax_all.set_ylabel("PER (%)", color=INK2, fontsize=11)
    ax_all.set_ylim(-2, 60)
    ax_all.legend(loc="upper left", bbox_to_anchor=(1.01, 1.0), frameon=False,
                  fontsize=9.5, labelcolor=INK2)
    ax_zoom.set_title(f"Packet error rate, 0 to {ZOOM} ms", loc="left", color=INK2, fontsize=11)
    ax_zoom.set_ylabel("PER (%)", color=INK2, fontsize=11)
    ax_zoom.set_ylim(-2, 60)
    ax_zoom.set_xlim(-0.01, ZOOM + 0.01)
    ax_swap.set_title("Swap time, median of each spacing", loc="left", color=INK2, fontsize=11)
    ax_swap.set_ylabel("ms", color=INK2, fontsize=11)
    ax_swap.set_ylim(0, None)
    ax_swap.set_xlabel("Inter-frame spacing, frame end to next frame start (ms)",
                       color=INK2, fontsize=11)

    # Direct labels on the swap times, at the right, spread apart.
    ends = sorted((median(d["swap"]), label, color) for _, label, color, _, d in data)
    lo, hi = 0, max(e[0] for e in ends) * 1.15
    ax_swap.set_ylim(lo, hi)
    last = -1.0
    for y, label, color in ends:
        y_text = max(y, last + 0.1 * hi)
        last = y_text
        x_end = max(d["ifs"][-1] for *_, d in data)
        ax_swap.annotate(f"  {y:.3f} ms  {label}", xy=(x_end, y), xytext=(x_end, y_text),
                         va="center", fontsize=8.5, color=INK2, annotation_clip=False)

    fig.suptitle("Receiver replaced after every frame, no radio (software IFS)",
                 x=0.06, y=0.99, ha="left", color=INK, fontsize=14, fontweight="bold")
    first = data[0][0]
    with open(out / f"{first}.csv") as f:
        row = next(csv.DictReader(f))
        frames = int(row["sent_h"]) + int(row["sent_z"])
    ifs = data[0][4]["ifs"]
    step = min(b - a for a, b in zip(ifs, ifs[1:])) if len(ifs) > 1 else 0
    fig.text(0.06, 0.955,
             f"Recorded 802.11ah and 802.15.4 frames cut to the frame, replayed at 4 MSps, "
             f"{frames} per spacing, every {step:.3g} ms.\n{system}",
             ha="left", va="top", color=MUTED, fontsize=9.5, linespacing=1.4)
    fig.tight_layout(rect=(0, 0, 0.72, 0.93))
    png = out / "bench.png"
    fig.savefig(png, dpi=150, facecolor=SURFACE)

    lines = [
        "| Swap | Swap time (median) | PER ≤ 1 % from | PER above 1 ms (mean) | Samples dropped |",
        "|------|--------------------|----------------|-----------------------|-----------------|",
    ]
    for _, label, _, _, d in data:
        at = edge(d)
        above = [p for i, p in zip(d["ifs"], d["per"]) if i > 1.0]
        lines.append(
            f"| {label} | {median(d['swap']):.3f} ms | "
            f"{'–' if at is None else f'{at:.2f} ms'} | "
            f"{sum(above) / len(above) if above else float('nan'):.2f} % | {d['dropped']} |"
        )
    (out / "summary.md").write_text(
        f"{system}\n\n" + "\n".join(lines) + "\n\n"
        "Samples dropped: by the replay (its output full for longer than a radio's\n"
        "buffers last) and by the link to the receiver (full); what a radio would\n"
        "have lost to a receiver that fell behind. Not 0: the run was CPU-bound.\n"
    )
    print(png)
    print("\n".join(lines))


main()
