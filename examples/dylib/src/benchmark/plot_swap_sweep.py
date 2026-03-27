#!/usr/bin/env python3
"""
Plot swap benchmark: N×FFT → terminate → rebuild N×IFFT.

CSV: name,n_blocks,fft_size,n_samples,phase1_run_s,terminate_s,import_s,add_fg_s,connect_s,rt_create_s,fg_init_s,fg_exec_s

Views:
  1. Interruption time breakdown (stacked bars) — one figure per n_samples
  2. Interruption time vs block count (line plot per FFT size) — one figure per n_samples
  3. Full cycle comparison: phase1 + interruption + phase2 — one figure per n_samples
  4. Interruption time vs n_samples (line plot per block count, fixed FFT size)

Usage: python3 plot_swap_sweep.py results/swap_sweep.txt [output_dir]
"""

import sys
import csv
import numpy as np
import matplotlib.pyplot as plt
from collections import defaultdict
from pathlib import Path

SWAP_STAGES = ["terminate", "import", "add_fg", "connect", "rt_create", "fg_init"]
SWAP_LABELS = ["Terminate", "Import .so", "Add to FG", "Connect", "RT Create", "FG Init"]
SWAP_COLORS = ["#9B59B6", "#E74C3C", "#F39C12", "#2ECC71", "#3498DB", "#1ABC9C"]
ALL_STAGES = ["phase1_run", "terminate", "import", "add_fg", "connect", "rt_create", "fg_init", "fg_exec"]


def load_data(path):
    """Load CSV, return dict keyed by (method, n_blocks, fft_size, n_samples) → {stage: [values_ms]}."""
    groups = defaultdict(lambda: defaultdict(list))
    with open(path) as f:
        for row in csv.reader(f):
            if len(row) < 12:
                continue
            method = row[0].strip()
            n_blocks = int(row[1])
            fft_size = int(row[2])
            n_samples = int(row[3])
            key = (method, n_blocks, fft_size, n_samples)
            vals_ms = [float(x) * 1000 for x in row[4:12]]
            for stage, val in zip(ALL_STAGES, vals_ms):
                groups[key][stage].append(val)
    return groups


def get_mean(data, method, blocks, fft_size, n_samples, stage):
    key = (method, blocks, fft_size, n_samples)
    if key in data and stage in data[key]:
        return np.mean(data[key][stage])
    return np.nan


def get_std(data, method, blocks, fft_size, n_samples, stage):
    key = (method, blocks, fft_size, n_samples)
    if key in data and stage in data[key]:
        return np.std(data[key][stage])
    return np.nan


def get_interruption(data, method, blocks, fft_size, n_samples):
    return sum(get_mean(data, method, blocks, fft_size, n_samples, s) for s in SWAP_STAGES)


def get_interruption_std(data, method, blocks, fft_size, n_samples):
    key = (method, blocks, fft_size, n_samples)
    if key not in data:
        return np.nan
    n = len(data[key][SWAP_STAGES[0]])
    totals = [sum(data[key][s][r] for s in SWAP_STAGES) for r in range(n)]
    return np.std(totals)


def get_interruption_percentiles(data, method, blocks, fft_size, n_samples):
    """Return (p16, p84) for interruption time — asymmetric ±1σ equivalent."""
    key = (method, blocks, fft_size, n_samples)
    if key not in data:
        return np.nan, np.nan
    n = len(data[key][SWAP_STAGES[0]])
    totals = [sum(data[key][s][r] for s in SWAP_STAGES) for r in range(n)]
    return np.percentile(totals, 15.87), np.percentile(totals, 84.13)


def get_percentiles(data, method, blocks, fft_size, n_samples, stage):
    """Return (p16, p84) for a single stage."""
    key = (method, blocks, fft_size, n_samples)
    if key in data and stage in data[key]:
        vals = data[key][stage]
        return np.percentile(vals, 15.87), np.percentile(vals, 84.13)
    return np.nan, np.nan


# ─── View 1: Interruption breakdown (stacked bars) ────────────────────────

def plot_interruption_breakdown(data, block_counts, fft_sizes, n_samples_list, methods, out_dir):
    for ns in n_samples_list:
        n_configs = len(block_counts)
        fig, axes = plt.subplots(1, len(fft_sizes), figsize=(6 * len(fft_sizes), 6))
        if len(fft_sizes) == 1:
            axes = [axes]
        fig.suptitle(f"Swap Interruption Breakdown  |  n_samples={ns}",
                     fontsize=14, fontweight="bold")

        for ax, fft_size in zip(axes, fft_sizes):
            # Skip if no data for this (fft_size, n_samples) combo
            has_data = any((m, b, fft_size, ns) in data for m in methods for b in block_counts)
            if not has_data:
                ax.set_title(f"FFT={fft_size}\n(no data)")
                continue

            x = np.arange(n_configs)
            width = 0.35

            for mi, method in enumerate(methods):
                bottoms = np.zeros(n_configs)
                for si, (stage, label, color) in enumerate(zip(SWAP_STAGES, SWAP_LABELS, SWAP_COLORS)):
                    means = np.array([get_mean(data, method, b, fft_size, ns, stage) for b in block_counts])
                    means = np.nan_to_num(means)
                    alpha = 0.85 if mi == 0 else 0.55
                    offset = -width / 2 + mi * width
                    ax.bar(x + offset, means, width, bottom=bottoms,
                           label=f"{label}" if (mi == 0 and ax == axes[0]) else "",
                           color=color, alpha=alpha,
                           edgecolor="black" if mi == 1 else "none", linewidth=0.5)
                    bottoms += means

                for j, total in enumerate(bottoms):
                    if total > 0:
                        offset = -width / 2 + mi * width
                        ax.text(x[j] + offset, total, f"{total:.2f}",
                                ha="center", va="bottom", fontsize=7, fontweight="bold")

            ax.set_title(f"FFT size = {fft_size}")
            ax.set_xticks(x)
            ax.set_xticklabels([str(b) for b in block_counts])
            ax.set_xlabel("Block Count")
            ax.set_ylabel("Interruption Time (ms)")
            ax.grid(axis="y", alpha=0.3)

        from matplotlib.patches import Patch
        legend_elements = [Patch(facecolor=c, label=l) for c, l in zip(SWAP_COLORS, SWAP_LABELS)]
        legend_elements += [Patch(facecolor="gray", alpha=0.85, label="dynv2"),
                            Patch(facecolor="gray", alpha=0.55, edgecolor="black", linewidth=0.5, label="static")]
        axes[-1].legend(handles=legend_elements, fontsize=8, loc="upper left")

        plt.tight_layout()
        path = out_dir / f"swap_view1_breakdown_ns{ns}.png"
        plt.savefig(path, dpi=150, bbox_inches="tight")
        print(f"Saved: {path}")
        plt.close()


# ─── View 2: Interruption time vs block count ─────────────────────────────

def plot_interruption_vs_blocks(data, block_counts, fft_sizes, n_samples_list, methods, out_dir):
    for ns in n_samples_list:
        fig, ax = plt.subplots(figsize=(10, 6))
        fig.suptitle(f"Swap Interruption Time vs Block Count  |  n_samples={ns}",
                     fontsize=14, fontweight="bold")

        cmap = plt.cm.viridis
        colors = [cmap(i / max(1, len(fft_sizes) - 1)) for i in range(len(fft_sizes))]
        bx = np.array(block_counts)

        for fi, (fft_size, color) in enumerate(zip(fft_sizes, colors)):
            for mi, method in enumerate(methods):
                means = np.array([get_interruption(data, method, b, fft_size, ns) for b in block_counts])
                pcts = [get_interruption_percentiles(data, method, b, fft_size, ns) for b in block_counts]
                lo = np.array([p[0] for p in pcts])
                hi = np.array([p[1] for p in pcts])
                if np.all(np.isnan(means)):
                    continue
                style = "o-" if "dynv2" in method else "s--"
                alpha = 1.0 if "dynv2" in method else 0.6
                label = f"FFT {fft_size}" if mi == 0 else ""
                ax.plot(bx, means, style, color=color, alpha=alpha, label=label)
                fill_alpha = 0.15 if "dynv2" in method else 0.08
                ax.fill_between(bx, lo, hi, color=color, alpha=fill_alpha)

        ax.set_xlabel("Block Count")
        ax.set_ylabel("Interruption Time (ms)")
        ax.set_xscale("log", base=2)
        ax.legend(fontsize=9)
        ax.grid(alpha=0.3)
        ax.set_title("solid=dynv2, dashed=static  |  shaded=±1 std")

        plt.tight_layout()
        path = out_dir / f"swap_view2_vs_blocks_ns{ns}.png"
        plt.savefig(path, dpi=150, bbox_inches="tight")
        print(f"Saved: {path}")
        plt.close()


# ─── View 3: Full cycle timeline ──────────────────────────────────────────

def plot_full_cycle(data, block_counts, fft_sizes, n_samples_list, methods, out_dir):
    cycle_stages = ALL_STAGES
    cycle_labels = ["Phase 1\n(N×FFT)", "Terminate", "Import\n.so", "Add to\nFG", "Connect", "RT\nCreate", "FG\nInit", "FG Exec\n(N×IFFT)"]
    cycle_colors = ["#3498DB", "#9B59B6", "#E74C3C", "#F39C12", "#2ECC71", "#1F77B4", "#1ABC9C", "#17BECF"]

    for ns in n_samples_list:
        mid_fft = fft_sizes[len(fft_sizes) // 2] if len(fft_sizes) > 1 else fft_sizes[0]
        selected = [(block_counts[0], mid_fft), (block_counts[-1], mid_fft)]
        if len(block_counts) > 2:
            selected.insert(1, (block_counts[len(block_counts) // 2], mid_fft))

        # Filter to configs that have data for this n_samples
        selected = [(b, f) for (b, f) in selected
                    if any((m, b, f, ns) in data for m in methods)]
        if not selected:
            continue

        fig, axes = plt.subplots(1, len(selected), figsize=(6 * len(selected), 6))
        if len(selected) == 1:
            axes = [axes]
        fig.suptitle(f"Full Swap Cycle  |  n_samples={ns}", fontsize=14, fontweight="bold")

        for ax, (blocks, fft_size) in zip(axes, selected):
            x = np.arange(len(methods))
            width = 0.5
            bottoms = np.zeros(len(methods))

            for si, (stage, label, color) in enumerate(zip(cycle_stages, cycle_labels, cycle_colors)):
                means = np.array([get_mean(data, m, blocks, fft_size, ns, stage) for m in methods])
                means = np.nan_to_num(means)
                ax.bar(x, means, width, bottom=bottoms,
                       label=label if ax == axes[0] else "",
                       color=color, alpha=0.85)
                for j, (m, bot) in enumerate(zip(means, bottoms)):
                    if m > 0.05:
                        ax.text(x[j], bot + m / 2, f"{m:.2f}", ha="center", va="center",
                                fontsize=7, fontweight="bold", color="white")
                bottoms += means

            for j, total in enumerate(bottoms):
                ax.text(x[j], total, f"{total:.1f}ms", ha="center", va="bottom",
                        fontsize=9, fontweight="bold")

            ax.set_title(f"{blocks} blocks, FFT={fft_size}")
            ax.set_xticks(x)
            method_labels = ["dynv2" if "dynv2" in m else "static" for m in methods]
            ax.set_xticklabels(method_labels, fontsize=10)
            ax.set_ylabel("Time (ms)")
            ax.grid(axis="y", alpha=0.3)

        axes[0].legend(fontsize=8, loc="upper left")
        plt.tight_layout()
        path = out_dir / f"swap_view3_full_cycle_ns{ns}.png"
        plt.savefig(path, dpi=150, bbox_inches="tight")
        print(f"Saved: {path}")
        plt.close()


# ─── View 4: Interruption + Execution vs n_samples ───────────────────────

def plot_vs_nsamples(data, block_counts, fft_sizes, n_samples_list, methods, out_dir):
    """Line plot: how phase1_run, fg_exec, and interruption scale with n_samples."""
    if len(n_samples_list) < 2:
        return

    for fft_size in fft_sizes:
        fig, axes = plt.subplots(1, 3, figsize=(18, 6))
        fig.suptitle(f"Time vs n_samples  |  FFT size={fft_size}", fontsize=14, fontweight="bold")

        cmap = plt.cm.tab10
        colors = [cmap(i / max(1, len(block_counts) - 1)) for i in range(len(block_counts))]
        ns_arr = np.array(n_samples_list)

        titles = ["Phase 1 (N×FFT run)", "Interruption Time", "Phase 2 (N×IFFT exec)"]
        for panel_idx, (ax, title) in enumerate(zip(axes, titles)):
            for bi, (blocks, color) in enumerate(zip(block_counts, colors)):
                for mi, method in enumerate(methods):
                    if panel_idx == 0:
                        vals = [get_mean(data, method, blocks, fft_size, ns, "phase1_run") for ns in n_samples_list]
                        pcts = [get_percentiles(data, method, blocks, fft_size, ns, "phase1_run") for ns in n_samples_list]
                    elif panel_idx == 1:
                        vals = [get_interruption(data, method, blocks, fft_size, ns) for ns in n_samples_list]
                        pcts = [get_interruption_percentiles(data, method, blocks, fft_size, ns) for ns in n_samples_list]
                    else:
                        vals = [get_mean(data, method, blocks, fft_size, ns, "fg_exec") for ns in n_samples_list]
                        pcts = [get_percentiles(data, method, blocks, fft_size, ns, "fg_exec") for ns in n_samples_list]

                    vals = np.array(vals)
                    lo = np.array([p[0] for p in pcts])
                    hi = np.array([p[1] for p in pcts])
                    if np.all(np.isnan(vals)):
                        continue
                    style = "o-" if "dynv2" in method else "s--"
                    alpha = 1.0 if "dynv2" in method else 0.6
                    label = f"{blocks} blocks" if (mi == 0 and panel_idx == 0) else ""
                    ns_x = ns_arr[:len(vals)]
                    ax.plot(ns_x, vals, style, color=color, alpha=alpha, label=label)
                    fill_alpha = 0.15 if "dynv2" in method else 0.08
                    ax.fill_between(ns_x, lo[:len(vals)], hi[:len(vals)], color=color, alpha=fill_alpha)

            ax.set_xlabel("n_samples")
            ax.set_ylabel("Time (ms)")
            ax.set_xscale("log", base=2)
            ax.set_title(title)
            ax.grid(alpha=0.3)

        axes[0].legend(fontsize=8)
        fig.text(0.5, -0.02, "solid=dynv2, dashed=static", ha="center", fontsize=10)
        plt.tight_layout()
        path = out_dir / f"swap_view4_vs_nsamples_fft{fft_size}.png"
        plt.savefig(path, dpi=150, bbox_inches="tight")
        print(f"Saved: {path}")
        plt.close()


# ─── Main ────────────────────────────────────────────────────────────────

def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <swap_sweep.txt> [output_dir]")
        sys.exit(1)

    data = load_data(sys.argv[1])
    out_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(sys.argv[1]).parent
    out_dir.mkdir(parents=True, exist_ok=True)

    block_counts = sorted(set(b for _, b, _, _ in data.keys()))
    fft_sizes = sorted(set(f for _, _, f, _ in data.keys()))
    n_samples_list = sorted(set(ns for _, _, _, ns in data.keys()))
    methods = sorted(set(m for m, _, _, _ in data.keys()))

    print(f"Methods:      {methods}")
    print(f"Block counts: {block_counts}")
    print(f"FFT sizes:    {fft_sizes}")
    print(f"n_samples:    {n_samples_list}")
    print(f"Data points:  {sum(len(v) for d in data.values() for v in d.values()) // len(ALL_STAGES)}")
    print()

    plot_interruption_breakdown(data, block_counts, fft_sizes, n_samples_list, methods, out_dir)
    plot_interruption_vs_blocks(data, block_counts, fft_sizes, n_samples_list, methods, out_dir)
    plot_full_cycle(data, block_counts, fft_sizes, n_samples_list, methods, out_dir)
    plot_vs_nsamples(data, block_counts, fft_sizes, n_samples_list, methods, out_dir)

    # Stats table
    print("\n" + "=" * 160)
    print(f"{'Method':<14} {'Blocks':>6} {'FFT':>5} {'nSamples':>10} {'Phase1':>9} {'Term':>9} {'Import':>9} "
          f"{'AddFG':>9} {'Connect':>9} {'RTCreate':>9} {'FGInit':>9} {'FGExec':>9} {'Interr':>9}")
    print("=" * 160)
    for ns in n_samples_list:
        for b in block_counts:
            for f in fft_sizes:
                for m in methods:
                    key = (m, b, f, ns)
                    if key not in data:
                        continue
                    means = {s: np.mean(data[key][s]) for s in ALL_STAGES}
                    interr = sum(means[s] for s in SWAP_STAGES)
                    short_m = "dynv2" if "dynv2" in m else "static"
                    print(f"{short_m:<14} {b:>6} {f:>5} {ns:>10} {means['phase1_run']:>8.3f} "
                          f"{means['terminate']:>8.3f} {means['import']:>8.3f} "
                          f"{means['add_fg']:>8.3f} {means['connect']:>8.3f} "
                          f"{means['rt_create']:>8.3f} {means['fg_init']:>8.3f} "
                          f"{means['fg_exec']:>8.3f} {interr:>8.3f}")
    print("=" * 160)


if __name__ == "__main__":
    main()
