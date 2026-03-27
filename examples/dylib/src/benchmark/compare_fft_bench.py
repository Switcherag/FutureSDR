#!/usr/bin/env python3
"""Compare FFT benchmark results: fft_static vs fft_dynv2 across different block counts.

CSV format: name,fft_pairs,fft_size,n_samples,import_s,add_fg_s,connect_s,runtime_s
"""

import sys
import csv
import numpy as np
import matplotlib.pyplot as plt
from collections import defaultdict

def load_data(path):
    """Load CSV data, grouping by (name, fft_pairs)."""
    groups = defaultdict(lambda: defaultdict(list))
    with open(path) as f:
        for row in csv.reader(f):
            if len(row) < 8:
                continue
            name = row[0].strip()
            fft_pairs = int(row[1])
            stages = [float(x) * 1000 for x in row[4:8]]  # convert to ms
            groups[(name, fft_pairs)]["import"].append(stages[0])
            groups[(name, fft_pairs)]["add_fg"].append(stages[1])
            groups[(name, fft_pairs)]["connect"].append(stages[2])
            groups[(name, fft_pairs)]["runtime"].append(stages[3])
    return groups

def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <results_file> [output_png]")
        sys.exit(1)

    data = load_data(sys.argv[1])
    out_path = sys.argv[2] if len(sys.argv) > 2 else "fft_bench_comparison.png"

    # Discover unique pair counts and methods
    methods = sorted(set(name for name, _ in data.keys()))
    pair_counts = sorted(set(pc for _, pc in data.keys()))
    stages = ["import", "add_fg", "connect", "runtime"]
    stage_labels = ["Import\nBlocks", "Add to\nFlowgraph", "Connect\nBlocks", "Runtime\nExecution"]
    colors = {"fft_static": "#4C72B0", "fft_dynv2": "#DD8452"}

    fig, axes = plt.subplots(1, 3, figsize=(18, 6))
    fig.suptitle("FFT Benchmark: Static vs Dynamic Plugin (v2)", fontsize=14, fontweight="bold")

    for ax_idx, pc in enumerate(pair_counts):
        ax = axes[ax_idx]
        n_blocks = pc * 2  # IFFT + FFT per pair

        x = np.arange(len(stages))
        width = 0.35

        for i, method in enumerate(methods):
            key = (method, pc)
            if key not in data:
                continue
            means = [np.mean(data[key][s]) for s in stages]
            stds = [np.std(data[key][s]) for s in stages]
            offset = -width/2 + i * width
            bars = ax.bar(x + offset, means, width, yerr=stds, label=method,
                         color=colors.get(method, f"C{i}"), capsize=3, alpha=0.85)
            for bar, m in zip(bars, means):
                if m > 0.01:
                    ax.text(bar.get_x() + bar.get_width()/2, bar.get_height(),
                           f"{m:.2f}", ha="center", va="bottom", fontsize=7)

        ax.set_title(f"{n_blocks} FFT blocks ({pc} IFFT→FFT pairs)")
        ax.set_xticks(x)
        ax.set_xticklabels(stage_labels)
        ax.set_ylabel("Time (ms)")
        ax.legend()
        ax.grid(axis="y", alpha=0.3)

    plt.tight_layout()
    plt.savefig(out_path, dpi=150, bbox_inches="tight")
    print(f"Saved: {out_path}")

    # Also generate a total time comparison
    fig2, ax2 = plt.subplots(figsize=(10, 6))
    fig2.suptitle("FFT Benchmark: Total Time Comparison", fontsize=14, fontweight="bold")

    x = np.arange(len(pair_counts))
    width = 0.35

    for i, method in enumerate(methods):
        totals_mean = []
        totals_std = []
        for pc in pair_counts:
            key = (method, pc)
            if key not in data:
                totals_mean.append(0)
                totals_std.append(0)
                continue
            # Compute total per run
            n_runs = len(data[key]["import"])
            run_totals = [sum(data[key][s][r] for s in stages) for r in range(n_runs)]
            totals_mean.append(np.mean(run_totals))
            totals_std.append(np.std(run_totals))

        bars = ax2.bar(x + (-width/2 + i * width), totals_mean, width, yerr=totals_std,
                      label=method, color=colors.get(method, f"C{i}"), capsize=4, alpha=0.85)
        for bar, m in zip(bars, totals_mean):
            ax2.text(bar.get_x() + bar.get_width()/2, bar.get_height(),
                    f"{m:.1f}ms", ha="center", va="bottom", fontsize=9)

    ax2.set_xticks(x)
    ax2.set_xticklabels([f"{pc*2} blocks\n({pc} pairs)" for pc in pair_counts])
    ax2.set_ylabel("Total Time (ms)")
    ax2.set_xlabel("Number of FFT blocks")
    ax2.legend()
    ax2.grid(axis="y", alpha=0.3)

    total_path = out_path.replace(".png", "_total.png")
    plt.tight_layout()
    plt.savefig(total_path, dpi=150, bbox_inches="tight")
    print(f"Saved: {total_path}")

    # Print stats table
    print("\n" + "="*90)
    print(f"{'Method':<15} {'Pairs':>6} {'Import':>10} {'AddFG':>10} {'Connect':>10} {'Runtime':>10} {'Total':>10}")
    print("="*90)
    for pc in pair_counts:
        for method in methods:
            key = (method, pc)
            if key not in data:
                continue
            means = {s: np.mean(data[key][s]) for s in stages}
            total = sum(means.values())
            print(f"{method:<15} {pc:>6} {means['import']:>9.3f} {means['add_fg']:>9.3f} "
                  f"{means['connect']:>9.3f} {means['runtime']:>9.3f} {total:>9.3f}")
    print("="*90)

if __name__ == "__main__":
    main()
