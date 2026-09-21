#!/usr/bin/env python3
"""PER and swap time against the software IFS, for four receiver swaps.

Reads the replay's CSVs (one per swap, `--retune-us 0`: no radio, the swap
alone) from this directory and writes software_ifs.png:

    python3 plot_software_ifs.py
"""
import csv
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

HERE = Path(__file__).parent

# Categorical slots 1-4 in order (validated: scripts/validate_palette.js of
# the dataviz method); markers carry identity too.
SERIES = [
    ("zz.csv", "ZigBee → ZigBee", "#2a78d6", "o"),
    ("hh_simple.csv", "HaLow simple → simple", "#eb6834", "s"),
    ("hh_granular.csv", "HaLow granular → granular", "#1baf7a", "^"),
    ("zh_simple.csv", "ZigBee ⇄ HaLow simple", "#eda100", "D"),
]
SURFACE, GRID, AXIS = "#fcfcfb", "#e1e0d9", "#c3c2b7"
INK, INK2, MUTED = "#0b0b0b", "#52514e", "#898781"
XMAX = 1.0


def load(name):
    rows = list(csv.DictReader(open(HERE / name)))
    # Above 1 ms every swap is done in time.
    rows = [r for r in rows if float(r["ifs_ms"]) <= XMAX]
    rows.sort(key=lambda r: float(r["ifs_ms"]))
    ifs = [float(r["ifs_ms"]) for r in rows]
    per = [100 * float(r["per"]) for r in rows]
    swap = [float(r["swap_median_ms"]) for r in rows]
    return ifs, per, swap


fig, (ax_per, ax_swap) = plt.subplots(
    2, 1, figsize=(9, 8), sharex=True, gridspec_kw={"height_ratios": [3, 2]}
)
fig.patch.set_facecolor(SURFACE)
for ax in (ax_per, ax_swap):
    ax.set_facecolor(SURFACE)
    ax.grid(True, color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(AXIS)
    ax.tick_params(colors=INK2, labelsize=10)

ends = []
for name, label, color, marker in SERIES:
    if not (HERE / name).exists():
        continue
    ifs, per, swap = load(name)
    style = dict(color=color, linewidth=2, marker=marker, markersize=5,
                 markeredgecolor=SURFACE, markeredgewidth=1.2, label=label)
    ax_per.plot(ifs, per, **style)
    ax_swap.plot(ifs, swap, **style)
    ends.append((swap[-1], label, color))

ax_per.set_ylabel("Packet error rate (%)", color=INK2, fontsize=11)
ax_per.set_ylim(-2, 65)
ax_swap.set_ylabel("Swap time, median (ms)", color=INK2, fontsize=11)
ax_swap.set_ylim(0, None)
ax_swap.set_xlabel("Inter-frame spacing, frame end to next frame start (ms)", color=INK2, fontsize=11)
ax_swap.set_xlim(-0.02, XMAX + 0.02)

# Direct labels at the right end of the swap-time lines, spread apart.
ends.sort()
last = -1.0
lo, hi = ax_swap.get_ylim()
gap = 0.06 * (hi - lo)
for y, label, color in ends:
    y = max(y, last + gap)
    last = y
    ax_swap.annotate(label, xy=(XMAX, y), xytext=(8, 0), textcoords="offset points",
                     va="center", fontsize=9, color=INK2)

ax_per.legend(loc="upper right", frameon=False, fontsize=10, labelcolor=INK2)
fig.suptitle("Receiver replaced after every frame, no radio (software IFS)",
             x=0.07, y=0.985, ha="left", color=INK, fontsize=14, fontweight="bold")
fig.text(0.07, 0.945,
         "Recorded 802.15.4 and 802.11ah frames cut to the frame, replayed at 4 MSps, "
         "400 per spacing.\nIFS from the end of a frame to the start of the next. "
         "Controller::replace; 4 runtime threads pinned\nto performance cores, the replay "
         "on a fifth, all kept awake (--cpus auto --keep-awake).",
         ha="left", va="top", color=MUTED, fontsize=10, linespacing=1.4)
fig.tight_layout(rect=(0, 0, 0.86, 0.87))
out = HERE / "software_ifs.png"
fig.savefig(out, dpi=150, facecolor=SURFACE)
print(out)
