#!/usr/bin/env python3
"""The granular HaLow receiver replaced whole, or only its decoder.

Reads hh_granular.csv (wlan_granular.toml replaced by itself after every
frame) and hh_granular_decoder.csv (wlan_granular_viterbi.toml and
wlan_granular_hard.toml in turn: the decoder alone is replaced) and writes
block_swap.png:

    python3 plot_block_swap.py
"""
import csv
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

HERE = Path(__file__).parent
XMAX = 1.0

# Categorical slots 1-2 (validated with the dataviz method's script).
SERIES = [
    ("hh_granular.csv", "Whole receiver replaced (13 blocks)", "#2a78d6", "o"),
    ("hh_granular_decoder.csv", "Decoder only replaced (Viterbi ⇄ hard)", "#eb6834", "s"),
]
SURFACE, GRID, AXIS = "#fcfcfb", "#e1e0d9", "#c3c2b7"
INK, INK2, MUTED = "#0b0b0b", "#52514e", "#898781"


def load(name):
    rows = [r for r in csv.DictReader(open(HERE / name)) if float(r["ifs_ms"]) <= XMAX]
    rows.sort(key=lambda r: float(r["ifs_ms"]))
    return (
        [float(r["ifs_ms"]) for r in rows],
        [100 * float(r["per"]) for r in rows],
        [float(r["swap_median_ms"]) for r in rows],
    )


fig, (ax_per, ax_swap) = plt.subplots(
    2, 1, figsize=(9, 7.5), sharex=True, gridspec_kw={"height_ratios": [3, 2]}
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

for name, label, color, marker in SERIES:
    ifs, per, swap = load(name)
    style = dict(color=color, linewidth=2, marker=marker, markersize=5,
                 markeredgecolor=SURFACE, markeredgewidth=1.2, label=label)
    ax_per.plot(ifs, per, **style)
    ax_swap.plot(ifs, swap, **style)
    ax_swap.annotate(f"{sorted(swap)[len(swap) // 2]:.3f} ms", xy=(XMAX, swap[-1]),
                     xytext=(8, 0), textcoords="offset points", va="center",
                     fontsize=9, color=INK2)

ax_per.set_ylabel("Packet error rate (%)", color=INK2, fontsize=11)
ax_per.set_ylim(-2, 55)
ax_per.legend(loc="upper right", frameon=False, fontsize=10, labelcolor=INK2)
ax_swap.set_ylabel("Swap time, median (ms)", color=INK2, fontsize=11)
ax_swap.set_ylim(0, None)
ax_swap.set_xlabel("Inter-frame spacing, frame end to next frame start (ms)",
                   color=INK2, fontsize=11)
ax_swap.set_xlim(-0.02, XMAX + 0.02)
fig.suptitle("Replacing one block instead of the whole receiver",
             x=0.07, y=0.985, ha="left", color=INK, fontsize=14, fontweight="bold")
fig.text(0.07, 0.945,
         "wlan_granular HaLow receiver, replaced after every frame; recorded frames at 4 MSps,\n"
         "400 per spacing, no radio. The decoder runs as a flowgraph of its own (swappable),\n"
         "linked to the rest, which keeps running (--cpus auto --keep-awake).",
         ha="left", va="top", color=MUTED, fontsize=10, linespacing=1.4)
fig.tight_layout(rect=(0, 0, 0.9, 0.86))
out = HERE / "block_swap.png"
fig.savefig(out, dpi=150, facecolor=SURFACE)
print(out)
