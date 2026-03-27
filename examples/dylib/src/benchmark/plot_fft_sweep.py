#!/usr/bin/env python3
"""
Plot FFT benchmark sweep results with multiple visualization views.

CSV format: name,fft_pairs,fft_size,n_samples,import_s,add_fg_s,connect_s,runtime_s

Usage: python3 plot_fft_sweep.py results/fft_sweep.txt [output_dir]
"""

import sys
import csv
import numpy as np
import matplotlib.pyplot as plt
import matplotlib.colors as mcolors
from collections import defaultdict
from pathlib import Path

STAGES = ["import", "add_fg", "connect", "rt_create", "fg_init", "fg_exec"]
STAGE_LABELS = ["Import", "Add FG", "Connect", "RT Create", "FG Init", "FG Exec"]
STAGE_COLORS = ["#E74C3C", "#F39C12", "#2ECC71", "#3498DB", "#1ABC9C", "#9B59B6"]


def load_data(path):
    """Load CSV, return dict keyed by (method, blocks, fft_size) → {stage: [values_ms]}."""
    groups = defaultdict(lambda: defaultdict(list))
    with open(path) as f:
        for row in csv.reader(f):
            if len(row) < 10:
                continue
            method = row[0].strip()
            pairs = int(row[1])
            fft_size = int(row[2])
            blocks = pairs * 2
            vals_ms = [float(x) * 1000 for x in row[4:10]]
            key = (method, blocks, fft_size)
            for i, s in enumerate(STAGES):
                groups[key][s].append(vals_ms[i])
    return groups


def get_mean(data, method, blocks, fft_size, stage):
    key = (method, blocks, fft_size)
    if key in data and stage in data[key]:
        return np.mean(data[key][stage])
    return np.nan


def get_std(data, method, blocks, fft_size, stage):
    key = (method, blocks, fft_size)
    if key in data and stage in data[key]:
        return np.std(data[key][stage])
    return np.nan


def get_total(data, method, blocks, fft_size):
    return sum(get_mean(data, method, blocks, fft_size, s) for s in STAGES)


def get_total_std(data, method, blocks, fft_size):
    key = (method, blocks, fft_size)
    if key not in data:
        return np.nan
    n = len(data[key][STAGES[0]])
    totals = [sum(data[key][s][r] for s in STAGES) for r in range(n)]
    return np.std(totals)


def get_percentiles(data, method, blocks, fft_size, stage):
    """Return (p16, p84) — asymmetric ±1σ equivalent percentiles."""
    key = (method, blocks, fft_size)
    if key in data and stage in data[key]:
        vals = data[key][stage]
        return np.percentile(vals, 15.87), np.percentile(vals, 84.13)
    return np.nan, np.nan


def get_total_percentiles(data, method, blocks, fft_size):
    """Return (p16, p84) for the total across all stages."""
    key = (method, blocks, fft_size)
    if key not in data:
        return np.nan, np.nan
    n = len(data[key][STAGES[0]])
    totals = [sum(data[key][s][r] for s in STAGES) for r in range(n)]
    return np.percentile(totals, 15.87), np.percentile(totals, 84.13)


# ─── View 1: Heatmaps ───────────────────────────────────────────────────────

def plot_heatmaps(data, block_counts, fft_sizes, out_dir):
    fig, axes = plt.subplots(1, 3, figsize=(20, 6))
    fig.suptitle("Total Time Heatmaps (ms)", fontsize=14, fontweight="bold")

    for ax, method, title in zip(axes[:2],
                                  ["fft_dynv2", "fft_static"],
                                  ["Dynamic (dynv2)", "Static"]):
        grid = np.full((len(block_counts), len(fft_sizes)), np.nan)
        for i, b in enumerate(block_counts):
            for j, f in enumerate(fft_sizes):
                grid[i, j] = get_total(data, method, b, f)

        im = ax.imshow(grid, aspect="auto", cmap="YlOrRd")
        ax.set_xticks(range(len(fft_sizes)))
        ax.set_xticklabels(fft_sizes)
        ax.set_yticks(range(len(block_counts)))
        ax.set_yticklabels(block_counts)
        ax.set_xlabel("FFT Size")
        ax.set_ylabel("Block Count")
        ax.set_title(title)
        for i in range(len(block_counts)):
            for j in range(len(fft_sizes)):
                v = grid[i, j]
                if not np.isnan(v):
                    ax.text(j, i, f"{v:.1f}", ha="center", va="center", fontsize=7,
                            color="white" if v > grid[~np.isnan(grid)].max() * 0.5 else "black")
        fig.colorbar(im, ax=ax, shrink=0.8)

    # Overhead ratio heatmap
    ax = axes[2]
    ratio = np.full((len(block_counts), len(fft_sizes)), np.nan)
    for i, b in enumerate(block_counts):
        for j, f in enumerate(fft_sizes):
            dyn = get_total(data, "fft_dynv2", b, f)
            stat = get_total(data, "fft_static", b, f)
            if stat > 0:
                ratio[i, j] = (dyn - stat) / stat * 100

    vmax = max(abs(np.nanmin(ratio)), abs(np.nanmax(ratio)))
    im = ax.imshow(ratio, aspect="auto", cmap="RdYlGn_r",
                   norm=mcolors.TwoSlopeNorm(vcenter=0, vmin=-vmax, vmax=vmax))
    ax.set_xticks(range(len(fft_sizes)))
    ax.set_xticklabels(fft_sizes)
    ax.set_yticks(range(len(block_counts)))
    ax.set_yticklabels(block_counts)
    ax.set_xlabel("FFT Size")
    ax.set_ylabel("Block Count")
    ax.set_title("Overhead: (dynv2 − static) / static %")
    for i in range(len(block_counts)):
        for j in range(len(fft_sizes)):
            v = ratio[i, j]
            if not np.isnan(v):
                ax.text(j, i, f"{v:+.1f}%", ha="center", va="center", fontsize=7)
    fig.colorbar(im, ax=ax, shrink=0.8)

    plt.tight_layout()
    path = out_dir / "view1_heatmaps.png"
    plt.savefig(path, dpi=150, bbox_inches="tight")
    print(f"Saved: {path}")
    plt.close()


# ─── View 2: Time vs Block Count ─────────────────────────────────────────────

def plot_vs_blocks(data, block_counts, fft_sizes, out_dir):
    stages_plus = STAGES + ["total"]
    labels_plus = STAGE_LABELS + ["Total"]
    fig, axes = plt.subplots(1, len(stages_plus), figsize=(5 * len(stages_plus), 5))
    fig.suptitle("Time vs Block Count (solid=dynv2, dashed=static)", fontsize=14, fontweight="bold")

    cmap = plt.cm.viridis
    colors = [cmap(i / (len(fft_sizes) - 1)) for i in range(len(fft_sizes))]

    for ax, stage, label in zip(axes, stages_plus, labels_plus):
        for fi, (fft_size, color) in enumerate(zip(fft_sizes, colors)):
            dyn_means, dyn_lo, dyn_hi = [], [], []
            stat_means, stat_lo, stat_hi = [], [], []
            for b in block_counts:
                if stage == "total":
                    dyn_means.append(get_total(data, "fft_dynv2", b, fft_size))
                    lo, hi = get_total_percentiles(data, "fft_dynv2", b, fft_size)
                    dyn_lo.append(lo); dyn_hi.append(hi)
                    stat_means.append(get_total(data, "fft_static", b, fft_size))
                    lo, hi = get_total_percentiles(data, "fft_static", b, fft_size)
                    stat_lo.append(lo); stat_hi.append(hi)
                else:
                    dyn_means.append(get_mean(data, "fft_dynv2", b, fft_size, stage))
                    lo, hi = get_percentiles(data, "fft_dynv2", b, fft_size, stage)
                    dyn_lo.append(lo); dyn_hi.append(hi)
                    stat_means.append(get_mean(data, "fft_static", b, fft_size, stage))
                    lo, hi = get_percentiles(data, "fft_static", b, fft_size, stage)
                    stat_lo.append(lo); stat_hi.append(hi)

            dyn_means = np.array(dyn_means)
            dyn_lo, dyn_hi = np.array(dyn_lo), np.array(dyn_hi)
            stat_means = np.array(stat_means)
            stat_lo, stat_hi = np.array(stat_lo), np.array(stat_hi)
            bx = np.array(block_counts)

            ax.plot(bx, dyn_means, "o-", color=color, label=f"FFT {fft_size}" if ax == axes[0] else "")
            ax.fill_between(bx, dyn_lo, dyn_hi, color=color, alpha=0.15)
            ax.plot(bx, stat_means, "s--", color=color, alpha=0.6)
            ax.fill_between(bx, stat_lo, stat_hi, color=color, alpha=0.08)

        ax.set_xlabel("Block Count")
        ax.set_ylabel("Time (ms)")
        ax.set_title(label)
        ax.set_xscale("log", base=2)
        ax.grid(alpha=0.3)

    axes[0].legend(fontsize=7, loc="upper left")
    plt.tight_layout()
    path = out_dir / "view2_vs_blocks.png"
    plt.savefig(path, dpi=150, bbox_inches="tight")
    print(f"Saved: {path}")
    plt.close()


# ─── View 3: Time vs FFT Size ────────────────────────────────────────────────

def plot_vs_fft_size(data, block_counts, fft_sizes, out_dir):
    stages_plus = STAGES + ["total"]
    labels_plus = STAGE_LABELS + ["Total"]
    fig, axes = plt.subplots(1, len(stages_plus), figsize=(5 * len(stages_plus), 5))
    fig.suptitle("Time vs FFT Size (solid=dynv2, dashed=static)", fontsize=14, fontweight="bold")

    cmap = plt.cm.plasma
    colors = [cmap(i / (len(block_counts) - 1)) for i in range(len(block_counts))]

    for ax, stage, label in zip(axes, stages_plus, labels_plus):
        for bi, (blocks, color) in enumerate(zip(block_counts, colors)):
            dyn_means, dyn_lo, dyn_hi = [], [], []
            stat_means, stat_lo, stat_hi = [], [], []
            for f in fft_sizes:
                if stage == "total":
                    dyn_means.append(get_total(data, "fft_dynv2", blocks, f))
                    lo, hi = get_total_percentiles(data, "fft_dynv2", blocks, f)
                    dyn_lo.append(lo); dyn_hi.append(hi)
                    stat_means.append(get_total(data, "fft_static", blocks, f))
                    lo, hi = get_total_percentiles(data, "fft_static", blocks, f)
                    stat_lo.append(lo); stat_hi.append(hi)
                else:
                    dyn_means.append(get_mean(data, "fft_dynv2", blocks, f, stage))
                    lo, hi = get_percentiles(data, "fft_dynv2", blocks, f, stage)
                    dyn_lo.append(lo); dyn_hi.append(hi)
                    stat_means.append(get_mean(data, "fft_static", blocks, f, stage))
                    lo, hi = get_percentiles(data, "fft_static", blocks, f, stage)
                    stat_lo.append(lo); stat_hi.append(hi)

            dyn_means = np.array(dyn_means)
            dyn_lo, dyn_hi = np.array(dyn_lo), np.array(dyn_hi)
            stat_means = np.array(stat_means)
            stat_lo, stat_hi = np.array(stat_lo), np.array(stat_hi)
            fx = np.array(fft_sizes)

            ax.plot(fx, dyn_means, "o-", color=color, label=f"{blocks} blks" if ax == axes[0] else "")
            ax.fill_between(fx, dyn_lo, dyn_hi, color=color, alpha=0.15)
            ax.plot(fx, stat_means, "s--", color=color, alpha=0.6)
            ax.fill_between(fx, stat_lo, stat_hi, color=color, alpha=0.08)

        ax.set_xlabel("FFT Size")
        ax.set_ylabel("Time (ms)")
        ax.set_title(label)
        ax.set_xscale("log", base=2)
        ax.grid(alpha=0.3)

    axes[0].legend(fontsize=7, loc="upper left")
    plt.tight_layout()
    path = out_dir / "view3_vs_fft_size.png"
    plt.savefig(path, dpi=150, bbox_inches="tight")
    print(f"Saved: {path}")
    plt.close()


# ─── View 4: Overhead % bar chart ────────────────────────────────────────────

def plot_overhead(data, block_counts, fft_sizes, out_dir):
    fig, ax = plt.subplots(figsize=(14, 6))
    fig.suptitle("Dynamic Plugin Overhead: (dynv2 − static) / static %", fontsize=14, fontweight="bold")

    n_fft = len(fft_sizes)
    n_blocks = len(block_counts)
    x = np.arange(n_blocks)
    width = 0.8 / n_fft

    cmap = plt.cm.viridis
    colors = [cmap(i / (n_fft - 1)) for i in range(n_fft)]

    for fi, (fft_size, color) in enumerate(zip(fft_sizes, colors)):
        overheads = []
        for b in block_counts:
            dyn = get_total(data, "fft_dynv2", b, fft_size)
            stat = get_total(data, "fft_static", b, fft_size)
            overheads.append((dyn - stat) / stat * 100 if stat > 0 else 0)

        offset = -0.4 + fi * width + width / 2
        bars = ax.bar(x + offset, overheads, width, label=f"FFT {fft_size}",
                      color=color, alpha=0.85)

    ax.axhline(y=0, color="black", linewidth=0.5)
    ax.set_xticks(x)
    ax.set_xticklabels([str(b) for b in block_counts])
    ax.set_xlabel("Block Count")
    ax.set_ylabel("Overhead (%)")
    ax.legend(fontsize=8, ncol=3)
    ax.grid(axis="y", alpha=0.3)

    plt.tight_layout()
    path = out_dir / "view4_overhead.png"
    plt.savefig(path, dpi=150, bbox_inches="tight")
    print(f"Saved: {path}")
    plt.close()


# ─── View 5: Stage breakdown stacked bars ────────────────────────────────────

def plot_stage_breakdown(data, block_counts, fft_sizes, out_dir):
    # Pick representative configs: smallest/medium/largest block count × smallest/largest FFT
    selected_blocks = [block_counts[0], block_counts[len(block_counts) // 2], block_counts[-1]]
    selected_fft = [fft_sizes[0], fft_sizes[-1]]

    configs = [(b, f) for b in selected_blocks for f in selected_fft]
    methods = ["fft_dynv2", "fft_static"]

    fig, axes = plt.subplots(1, len(configs), figsize=(4 * len(configs), 6))
    fig.suptitle("Stage Breakdown: dynv2 vs static", fontsize=14, fontweight="bold")

    for ax, (blocks, fft_size) in zip(axes, configs):
        x = np.arange(len(methods))
        width = 0.5
        bottoms = np.zeros(len(methods))

        for si, (stage, label, color) in enumerate(zip(STAGES, STAGE_LABELS, STAGE_COLORS)):
            means = [get_mean(data, m, blocks, fft_size, stage) for m in methods]
            means = np.array(means)
            ax.bar(x, means, width, bottom=bottoms, label=label if ax == axes[0] else "",
                   color=color, alpha=0.85)
            for j, (m, bot) in enumerate(zip(means, bottoms)):
                total_h = bottoms.max() + means.max()
                if m > 0.02 * total_h:
                    ax.text(x[j], bot + m / 2, f"{m:.1f}", ha="center", va="center",
                            fontsize=7, fontweight="bold", color="white")
            bottoms += means

        for j, total in enumerate(bottoms):
            ax.text(x[j], total, f"{total:.1f}ms", ha="center", va="bottom",
                    fontsize=8, fontweight="bold")

        ax.set_title(f"{blocks} blks, FFT={fft_size}")
        ax.set_xticks(x)
        ax.set_xticklabels(["dynv2", "static"], fontsize=9)
        ax.set_ylabel("Time (ms)")
        ax.grid(axis="y", alpha=0.3)

    axes[0].legend(fontsize=8, loc="upper left")
    plt.tight_layout()
    path = out_dir / "view5_stage_breakdown.png"
    plt.savefig(path, dpi=150, bbox_inches="tight")
    print(f"Saved: {path}")
    plt.close()


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <fft_sweep.txt> [output_dir]")
        sys.exit(1)

    data = load_data(sys.argv[1])
    out_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(sys.argv[1]).parent
    out_dir.mkdir(parents=True, exist_ok=True)

    # Discover dimensions
    block_counts = sorted(set(b for _, b, _ in data.keys()))
    fft_sizes = sorted(set(f for _, _, f in data.keys()))
    methods = sorted(set(m for m, _, _ in data.keys()))

    print(f"Methods:      {methods}")
    print(f"Block counts: {block_counts}")
    print(f"FFT sizes:    {fft_sizes}")
    print(f"Data points:  {sum(len(v) for d in data.values() for v in d.values()) // len(STAGES)}")
    print()

    plot_heatmaps(data, block_counts, fft_sizes, out_dir)
    plot_vs_blocks(data, block_counts, fft_sizes, out_dir)
    plot_vs_fft_size(data, block_counts, fft_sizes, out_dir)
    plot_overhead(data, block_counts, fft_sizes, out_dir)
    plot_stage_breakdown(data, block_counts, fft_sizes, out_dir)

    # Stats table
    print("\n" + "=" * 130)
    print(f"{'Method':<12} {'Blocks':>6} {'FFT':>6} {'Import':>9} {'AddFG':>9} {'Connect':>9} "
          f"{'RTCreate':>9} {'FGInit':>9} {'FGExec':>9} {'Total':>9}")
    print("=" * 130)
    for b in block_counts:
        for f in fft_sizes:
            for m in methods:
                key = (m, b, f)
                if key not in data:
                    continue
                means = {s: np.mean(data[key][s]) for s in STAGES}
                total = sum(means.values())
                print(f"{m:<12} {b:>6} {f:>6} {means['import']:>8.3f} {means['add_fg']:>8.3f} "
                      f"{means['connect']:>8.3f} {means['rt_create']:>8.3f} "
                      f"{means['fg_init']:>8.3f} {means['fg_exec']:>8.3f} {total:>8.3f}")
    print("=" * 130)


if __name__ == "__main__":
    main()
