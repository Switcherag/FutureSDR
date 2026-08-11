#!/usr/bin/env python3
"""Square heatmap of channel-to-channel retune latency from `retune_matrix`.

Rows are the channel tuned *from*, columns the channel tuned *to*, and the cell
is how long `set_frequency` took to make that hop. Both channel plans are on
both axes, so the matrix has four quadrants and they answer different questions:

    Z→Z   in-band hops inside 2.4 GHz
    H→H   in-band hops inside 902-928 MHz
    Z→H   cross-band, the swap a dual-PHY receiver actually performs
    H→Z   cross-band, the return leg

If cross-band is intrinsically expensive, the two off-diagonal quadrants light
up uniformly. If instead the cost is contention with a running RX stream, the
whole matrix rises together and the quadrant structure stays flat — which is
why it is worth running with and without `--stream`.

Cells are drawn square (`aspect="equal"`) so distances read honestly.

Usage:
    python3 plot_retune_matrix.py [csv ...] [--out plot.png] [--log]

Several CSVs are shown side by side on a shared colour scale, which is how to
compare idle vs streaming, or two sample rates, without eyeballing two legends.
"""

import argparse
import csv
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import LogNorm, Normalize

MUTED = "#898781"
INK = "#0b0b0b"


def load(path):
    """Return `(labels, matrix)` with matrix[i][j] = ms for labels[i] → labels[j]."""
    with path.open(newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        sys.exit(f"{path}: empty")

    labels = []
    for r in rows:  # preserve capture order, which is the channel-plan order
        for k in ("from_label", "to_label"):
            if r[k] not in labels:
                labels.append(r[k])
    index = {lab: i for i, lab in enumerate(labels)}

    m = np.full((len(labels), len(labels)), np.nan)
    for r in rows:
        m[index[r["from_label"]], index[r["to_label"]]] = float(r["ms"])
    return labels, m


def plot(captures, out, use_log):
    finite = np.concatenate([m[np.isfinite(m)].ravel() for _, _, m in captures])
    if finite.size == 0:
        sys.exit("no finite measurements")
    lo = max(finite.min(), 1e-3) if use_log else finite.min()
    norm = LogNorm(vmin=lo, vmax=finite.max()) if use_log else Normalize(finite.min(), finite.max())

    fig, axes = plt.subplots(
        1, len(captures), figsize=(1.5 + 6.2 * len(captures), 7.0), layout="constrained",
        squeeze=False,
    )
    fig.patch.set_facecolor("#fcfcfb")

    for ax, (label, labels, m) in zip(axes[0], captures):
        im = ax.imshow(m, cmap="magma_r", norm=norm, aspect="equal",
                       interpolation="nearest", origin="upper")
        ax.set_title(label, color=INK, loc="left", fontsize=12)
        ax.set_xlabel("retune to", color=MUTED)
        ax.set_ylabel("retune from", color=MUTED)

        step = max(1, len(labels) // 21)
        ticks = range(0, len(labels), step)
        ax.set_xticks(list(ticks), [labels[i] for i in ticks], rotation=90, fontsize=7)
        ax.set_yticks(list(ticks), [labels[i] for i in ticks], fontsize=7)
        ax.tick_params(colors=MUTED, length=0)

        # Quadrant boundary: where the channel plan changes (Z... then H...).
        cut = next((i for i, l in enumerate(labels) if l[0] != labels[0][0]), None)
        if cut:
            for pos in (cut - 0.5,):
                ax.axhline(pos, color="#1f6fd0", linewidth=1.2)
                ax.axvline(pos, color="#1f6fd0", linewidth=1.2)
        for spine in ax.spines.values():
            spine.set_visible(False)

    cbar = fig.colorbar(im, ax=axes[0], shrink=0.82, pad=0.02)
    cbar.set_label("set_frequency duration (ms)", color=MUTED)
    cbar.ax.tick_params(colors=MUTED)
    cbar.outline.set_visible(False)

    if out:
        fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def quadrants(labels, m):
    """Median ms for each (from-plan, to-plan) quadrant."""
    plans = [l[0] for l in labels]
    out = {}
    for a in sorted(set(plans)):
        for b in sorted(set(plans)):
            rows = [i for i, p in enumerate(plans) if p == a]
            cols = [j for j, p in enumerate(plans) if p == b]
            block = m[np.ix_(rows, cols)]
            block = block[np.isfinite(block)]
            if block.size:
                out[f"{a}->{b}"] = (np.median(block), block.min(), block.max())
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="*", type=Path,
                   help="one or more retune_matrix CSVs, shown side by side")
    p.add_argument("--labels", help="comma-separated titles, one per CSV")
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--log", action="store_true",
                   help="log colour scale — use when idle and stalled hops differ by decades")
    args = p.parse_args()

    paths = args.csv or [Path("retune_matrix.csv")]
    for path in paths:
        if not path.exists():
            sys.exit(f"missing {path} — run retune_matrix first")
    titles = args.labels.split(",") if args.labels else [p.stem for p in paths]

    captures = []
    for path, title in zip(titles and paths, titles):
        labels, m = load(path)
        captures.append((title, labels, m))
        print(f"== {title}: {len(labels)} channels")
        for name, (med, lo, hi) in quadrants(labels, m).items():
            print(f"   {name}: median {med:8.3f} ms   min {lo:8.3f}   max {hi:8.3f}")

    plot(captures, args.out, args.log)


if __name__ == "__main__":
    main()
