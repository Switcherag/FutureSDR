#!/usr/bin/env python3
"""Quick-tune recall latency as the number of registered profiles grows.

One retune map per profile count: rows are the channel tuned *from*, columns the
channel tuned *to*, and the cell is how long the quick-tune recall took. All six
share one colour scale, so a panel that darkens has genuinely got slower.

The summary strip underneath is the same data reduced to one number per panel —
the median over the off-diagonal cells, i.e. hops that actually change frequency.
The diagonal is excluded throughout: recalling the channel you are already on is
not a retune, and costs about a third of one.

Panels are drawn at equal size rather than with square cells, because the whole
point is to compare a 2x2 against a 64x64. Cell *shape* carries no meaning here;
cell *colour* does.

Colour is a single-hue sequential ramp: the quantity is a magnitude, so lightness
alone should carry it. (No categorical palette is involved, so there is nothing
for the CVD validator to check.)

Usage:
    python3 plot_quicktune_profiles.py [--dir profiles_csv] [--out plot.png]

Reads `qt_n<N>.csv` for each N in --counts, as written by
`retune_matrix_brf --quick-tune --n-freqs N`.
"""

import argparse
import csv
import statistics
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import Normalize

INK = "#0b0b0b"
MUTED = "#898781"
GRID = "#e1e0d9"
SURFACE = "#fcfcfb"
# Single hue, light -> dark. Magnitude is carried by lightness alone.
RAMP = "Blues"
ACCENT = "#1f6fd0"


def load(path):
    """`(labels, matrix)` with matrix[i][j] = ms for labels[i] -> labels[j]."""
    with path.open(newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        sys.exit(f"{path}: empty")
    labels = []
    for r in rows:
        for k in ("from_label", "to_label"):
            if r[k] not in labels:
                labels.append(r[k])
    index = {lab: i for i, lab in enumerate(labels)}
    m = np.full((len(labels), len(labels)), np.nan)
    for r in rows:
        m[index[r["from_label"]], index[r["to_label"]]] = float(r["ms"])
    return labels, m


def off_diagonal(m):
    """Every cell that is a real change of frequency."""
    mask = ~np.eye(m.shape[0], dtype=bool)
    v = m[mask]
    return v[np.isfinite(v)]


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--dir", type=Path, default=Path("profiles_csv"))
    p.add_argument("--counts", default="2,4,8,16,32,64")
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    args = p.parse_args()

    counts = [int(c) for c in args.counts.split(",")]
    caps = []
    for n in counts:
        path = args.dir / f"qt_n{n}.csv"
        if not path.exists():
            sys.exit(f"missing {path} — run retune_matrix_brf --quick-tune --n-freqs {n}")
        labels, m = load(path)
        caps.append((n, labels, m, statistics.median(off_diagonal(m))))

    finite = np.concatenate([off_diagonal(m) for _, _, m, _ in caps])
    norm = Normalize(vmin=finite.min(), vmax=finite.max())

    fig = plt.figure(figsize=(12.5, 9.4), layout="constrained")
    fig.patch.set_facecolor(SURFACE)
    gs = fig.add_gridspec(3, 3, height_ratios=[1, 1, 0.75])

    im = None
    for k, (n, labels, m, med) in enumerate(caps):
        ax = fig.add_subplot(gs[k // 3, k % 3])
        im = ax.imshow(m, cmap=RAMP, norm=norm, aspect="auto",
                       interpolation="nearest", origin="upper")
        # Direct-label the one number that matters for this panel.
        ax.set_title(f"{n} profiles registered", color=INK, loc="left", fontsize=11)
        ax.text(0.5, -0.09, f"median recall {med:.2f} ms", transform=ax.transAxes,
                ha="center", va="top", color=MUTED, fontsize=10)
        ax.set_xticks([])
        ax.set_yticks([])
        for spine in ax.spines.values():
            spine.set_visible(False)

    cbar = fig.colorbar(im, ax=fig.axes, shrink=0.55, pad=0.015, location="right")
    cbar.set_label("quick-tune recall (ms)", color=MUTED)
    cbar.ax.tick_params(colors=MUTED)
    cbar.outline.set_visible(False)

    # Summary: the six medians, on a log x because the counts double.
    ax = fig.add_subplot(gs[2, :])
    ax.set_facecolor(SURFACE)
    xs = [c[0] for c in caps]
    ys = [c[3] for c in caps]
    ax.plot(xs, ys, color=ACCENT, linewidth=2, marker="o", markersize=8,
            markeredgecolor=SURFACE, markeredgewidth=1.5)
    for x, y in zip(xs, ys):
        ax.annotate(f"{y:.2f}", xy=(x, y), xytext=(0, 10),
                    textcoords="offset points", ha="center",
                    color=MUTED, fontsize=9)
    ax.set_xscale("log", base=2)
    ax.set_xticks(xs, [str(x) for x in xs])
    ax.set_xlabel("profiles registered", color=MUTED)
    ax.set_ylabel("median recall (ms)", color=MUTED)
    ax.set_title("Recall is bimodal: flat while profiles stay resident, "
                 "then one step, then flat again",
                 color=INK, loc="left", fontsize=12)
    ax.set_ylim(0, max(ys) * 1.35)
    ax.grid(True, color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    ax.tick_params(colors=MUTED)
    for side, spine in ax.spines.items():
        spine.set_visible(side in ("left", "bottom"))
        spine.set_color("#c3c2b7")

    print(f"{'profiles':>9}  {'median':>8}  {'min':>7}  {'max':>7}  pairs")
    for n, _, m, med in caps:
        v = off_diagonal(m)
        print(f"{n:>9}  {med:8.3f}  {v.min():7.3f}  {v.max():7.3f}  {v.size}")

    if args.out:
        fig.savefig(args.out, dpi=150)
        print(f"wrote {args.out}")
    else:
        plt.show()


if __name__ == "__main__":
    main()
