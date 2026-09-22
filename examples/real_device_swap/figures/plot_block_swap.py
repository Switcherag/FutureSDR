#!/usr/bin/env python3
"""Replacing the whole HaLow receiver, or one block of it.

    python3 plot_block_swap.py [RESULTS_DIR]

Reads bench.sh's gg, ss and 11 (the granular, simple and single-block
HaLow receivers, 13, 4 and 1 blocks, replaced whole) and gd (the granular
receiver's decoder alone replaced) from RESULTS_DIR (../results/laptop-6ms by
default) and writes block_swap.png here.
"""
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

from common_series import INK, INK2, MUTED, SERIES, SURFACE, load, style

HERE = Path(__file__).parent
RESULTS = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "../results/laptop-6ms"
XMAX = 1.0
LABELS = {
    "gg": "Whole receiver replaced (granular, 13 blocks)",
    "ss": "Whole receiver replaced (simple, 4 blocks)",
    "11": "Whole receiver replaced (single, 1 block)",
    "gd": "Decoder only replaced (inverse ⇄ Viterbi)",
}

fig, (ax_per, ax_swap) = plt.subplots(
    2, 1, figsize=(10, 7.5), sharex=True, gridspec_kw={"height_ratios": [3, 2]}
)
fig.patch.set_facecolor(SURFACE)
style(ax_per)
style(ax_swap)
ends = []

for key in ["gg", "ss", "11", "gd"]:
    if not (RESULTS / f"{key}.csv").exists():
        continue
    _, color, dash, marker = SERIES[key]
    ifs, per, swap = load(RESULTS / f"{key}.csv", XMAX)
    kw = dict(color=color, linewidth=1.8, linestyle=dash, marker=marker, markersize=4,
              markevery=5, markeredgecolor=SURFACE, markeredgewidth=0.8, label=LABELS[key])
    ax_per.plot(ifs, per, **kw)
    ax_swap.plot(ifs, swap, **kw)
    ends.append(sorted(swap)[len(swap) // 2])

ax_per.set_ylabel("Packet error rate (%)", color=INK2, fontsize=11)
ax_per.set_ylim(-2, 55)
ax_per.legend(loc="upper right", frameon=False, fontsize=10, labelcolor=INK2)
ax_swap.set_ylabel("Swap time, median (ms)", color=INK2, fontsize=11)
hi = max(ends) * 1.2
ax_swap.set_ylim(0, hi)
last = -1.0
for y in sorted(ends):
    y_text = max(y, last + 0.09 * hi)
    last = y_text
    ax_swap.annotate(f"  {y:.3f} ms", xy=(XMAX, y), xytext=(XMAX, y_text), va="center",
                     fontsize=9, color=INK2, annotation_clip=False)
ax_swap.set_xlabel("Inter-frame spacing, frame end to next frame start (ms)",
                   color=INK2, fontsize=11)
ax_swap.set_xlim(-0.02, XMAX + 0.02)
fig.suptitle("Replacing one block instead of the whole receiver",
             x=0.07, y=0.985, ha="left", color=INK, fontsize=14, fontweight="bold")
fig.text(0.07, 0.945,
         "HaLow receiver replaced after every frame; recorded frames at 4 MSps, 400 per spacing, "
         "every 0.01 ms from 0 to 6 ms, no radio;\nshown up to 1 ms. Decoder only: the decoder "
         "runs as a flowgraph of its own (swappable), the rest keeps running.",
         ha="left", va="top", color=MUTED, fontsize=9, linespacing=1.4)
fig.tight_layout(rect=(0, 0, 0.9, 0.9))
out = HERE / "block_swap.png"
fig.savefig(out, dpi=150, facecolor=SURFACE)
print(out)
