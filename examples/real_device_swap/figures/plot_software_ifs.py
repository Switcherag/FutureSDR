#!/usr/bin/env python3
"""PER and swap time against the software IFS, the receiver replaced by
itself or by the other PHY after every frame.

    python3 plot_software_ifs.py [RESULTS_DIR]

Reads bench.sh's CSVs (zz, ss, sz, gg, 11) from RESULTS_DIR
(../results/laptop-6ms by default) and writes software_ifs.png here.
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

fig, (ax_per, ax_swap) = plt.subplots(
    2, 1, figsize=(10, 8), sharex=True, gridspec_kw={"height_ratios": [3, 2]}
)
fig.patch.set_facecolor(SURFACE)
style(ax_per)
style(ax_swap)

ends = []
for key in ["zz", "ss", "sz", "gg", "11"]:
    if not (RESULTS / f"{key}.csv").exists():
        continue
    label, color, dash, marker = SERIES[key]
    ifs, per, swap = load(RESULTS / f"{key}.csv", XMAX)
    kw = dict(color=color, linewidth=1.8, linestyle=dash, marker=marker, markersize=4,
              markevery=5, markeredgecolor=SURFACE, markeredgewidth=0.8, label=label)
    ax_per.plot(ifs, per, **kw)
    ax_swap.plot(ifs, swap, **kw)
    ends.append((sorted(swap)[len(swap) // 2], label))

ax_per.set_ylabel("Packet error rate (%)", color=INK2, fontsize=11)
ax_per.set_ylim(-2, 55)
ax_per.legend(loc="upper right", frameon=False, fontsize=9.5, labelcolor=INK2)
ax_swap.set_ylabel("Swap time, median (ms)", color=INK2, fontsize=11)
ax_swap.set_xlabel("Inter-frame spacing, frame end to next frame start (ms)",
                   color=INK2, fontsize=11)
ax_swap.set_xlim(-0.02, XMAX + 0.02)
hi = max(y for y, _ in ends) * 1.2
ax_swap.set_ylim(0, hi)
last = -1.0
for y, label in sorted(ends):
    y_text = max(y, last + 0.09 * hi)
    last = y_text
    ax_swap.annotate(f"  {y:.3f} ms  {label}", xy=(XMAX, y), xytext=(XMAX, y_text),
                     va="center", fontsize=8.5, color=INK2, annotation_clip=False)

fig.suptitle("Receiver replaced after every frame, no radio (software IFS)",
             x=0.06, y=0.985, ha="left", color=INK, fontsize=14, fontweight="bold")
system = (RESULTS / "system.txt").read_text().splitlines()[0] if (RESULTS / "system.txt").exists() else ""
fig.text(0.06, 0.945,
         "Recorded 802.15.4 and 802.11ah frames cut to the frame, replayed at 4 MSps, 400 per "
         "spacing, every 0.01 ms from 0 to 6 ms;\nshown up to 1 ms, above which no frame is "
         f"lost.\n{system}",
         ha="left", va="top", color=MUTED, fontsize=9, linespacing=1.4)
fig.tight_layout(rect=(0, 0, 0.74, 0.88))
out = HERE / "software_ifs.png"
fig.savefig(out, dpi=150, facecolor=SURFACE)
print(out)
