#!/usr/bin/env python3
"""
Compare benchmark results for dyn, dynv2, and stat methods.

Reads CSV files with format: name,head_size,import_s,add_fg_s,connect_s,runtime_s
Produces a grouped bar chart + summary statistics for the 4 stages.

Usage:
    python3 compare_bench.py [--warmup N] FILE [FILE ...]
"""

import sys
import csv
import argparse
import numpy as np
import matplotlib.pyplot as plt
from collections import defaultdict, OrderedDict

STAGES = ["import", "add_fg", "connect", "runtime"]
STAGE_LABELS = [
    "Import blocks",
    "Add to flowgraph",
    "Connect blocks",
    "Runtime execution",
]


def load_results(filepaths, warmup=0):
    """Load CSV results from one or more files, skipping first `warmup` rows per method."""
    raw = defaultdict(list)
    for path in filepaths:
        with open(path) as f:
            reader = csv.reader(f)
            for row in reader:
                if len(row) < 6:
                    continue
                name = row[0].strip()
                values = [float(v) for v in row[2:6]]
                raw[name].append(values)

    data = defaultdict(lambda: defaultdict(list))
    for name, rows in raw.items():
        for values in rows[warmup:]:
            for stage, val in zip(STAGES, values):
                data[name][stage].append(val)

    return data


def print_stats(data):
    """Print summary statistics table with mean + median."""
    print(
        f"\n{'Method':<15} {'Stage':<20} {'Mean (ms)':>10} {'Median (ms)':>12} "
        f"{'Std (ms)':>10} {'Min (ms)':>10} {'Max (ms)':>10} {'N':>6}"
    )
    print("-" * 97)
    for method in sorted(data.keys()):
        for stage, label in zip(STAGES, STAGE_LABELS):
            vals = np.array(data[method][stage]) * 1000
            print(
                f"{method:<15} {label:<20} {vals.mean():>10.4f} {np.median(vals):>12.4f} "
                f"{vals.std():>10.4f} {vals.min():>10.4f} {vals.max():>10.4f} {len(vals):>6}"
            )
        total = sum(np.array(data[method][s]) for s in STAGES) * 1000
        print(
            f"{method:<15} {'TOTAL':<20} {total.mean():>10.4f} {np.median(total):>12.4f} "
            f"{total.std():>10.4f} {total.min():>10.4f} {total.max():>10.4f} {len(total):>6}"
        )
        print()


def plot_comparison(data, output="bench_comparison.png", warmup=0):
    """Grouped bar chart using median + IQR."""
    methods = sorted(data.keys())
    n_methods = len(methods)
    n_stages = len(STAGES)

    medians = np.zeros((n_methods, n_stages))
    q25 = np.zeros((n_methods, n_stages))
    q75 = np.zeros((n_methods, n_stages))
    for i, method in enumerate(methods):
        for j, stage in enumerate(STAGES):
            vals = np.array(data[method][stage]) * 1000
            medians[i, j] = np.median(vals)
            q25[i, j] = np.percentile(vals, 25)
            q75[i, j] = np.percentile(vals, 75)

    err_lo = medians - q25
    err_hi = q75 - medians

    # Compute total medians
    totals_median = np.zeros(n_methods)
    totals_err_lo = np.zeros(n_methods)
    totals_err_hi = np.zeros(n_methods)
    for i, method in enumerate(methods):
        total = sum(np.array(data[method][s]) for s in STAGES) * 1000
        totals_median[i] = np.median(total)
        totals_err_lo[i] = totals_median[i] - np.percentile(total, 25)
        totals_err_hi[i] = np.percentile(total, 75) - totals_median[i]

    x = np.arange(n_stages)
    width = 0.8 / n_methods

    warmup_label = f" (warmup={warmup})" if warmup else ""
    fig, axes = plt.subplots(1, 3, figsize=(20, 6))
    fig.suptitle(f"Median + IQR{warmup_label}", fontsize=11, y=0.98)

    # Left: all 4 stages
    ax = axes[0]
    for i, method in enumerate(methods):
        offset = (i - n_methods / 2 + 0.5) * width
        ax.bar(
            x + offset, medians[i], width,
            yerr=[err_lo[i], err_hi[i]], label=method, capsize=3
        )
    ax.set_xlabel("Stage")
    ax.set_ylabel("Time (ms)")
    ax.set_title("All stages")
    ax.set_xticks(x)
    ax.set_xticklabels(STAGE_LABELS, rotation=15, ha="right")
    ax.legend()
    ax.grid(axis="y", alpha=0.3)

    # Middle: only first 3 stages (without runtime)
    ax2 = axes[1]
    x2 = np.arange(n_stages - 1)
    for i, method in enumerate(methods):
        offset = (i - n_methods / 2 + 0.5) * width
        ax2.bar(
            x2 + offset, medians[i, :3], width,
            yerr=[err_lo[i, :3], err_hi[i, :3]], label=method, capsize=3,
        )
    ax2.set_xlabel("Stage")
    ax2.set_ylabel("Time (ms)")
    ax2.set_title("Setup stages only (without runtime)")
    ax2.set_xticks(x2)
    ax2.set_xticklabels(STAGE_LABELS[:3], rotation=15, ha="right")
    ax2.legend()
    ax2.grid(axis="y", alpha=0.3)

    # Right: total time
    ax3 = axes[2]
    x3 = np.arange(n_methods)
    colors = plt.rcParams["axes.prop_cycle"].by_key()["color"]
    for i, method in enumerate(methods):
        ax3.bar(
            x3[i], totals_median[i], 0.5,
            yerr=[[totals_err_lo[i]], [totals_err_hi[i]]],
            label=method, capsize=5, color=colors[i % len(colors)]
        )
    ax3.set_ylabel("Time (ms)")
    ax3.set_title("Total time (all stages)")
    ax3.set_xticks(x3)
    ax3.set_xticklabels(methods, rotation=15, ha="right")
    ax3.grid(axis="y", alpha=0.3)

    plt.tight_layout()
    plt.savefig(output, dpi=150)
    print(f"\nPlot saved to {output}")
    plt.show()


def main():
    parser = argparse.ArgumentParser(description="Compare benchmark results")
    parser.add_argument("files", nargs="+", help="CSV result files")
    parser.add_argument("--warmup", type=int, default=5,
                        help="Skip first N iterations per method as warmup (default: 5)")
    args = parser.parse_args()

    data = load_results(args.files, warmup=args.warmup)

    if not data:
        print("No data found.")
        sys.exit(1)

    print(f"Warmup: skipping first {args.warmup} iterations per method")
    print_stats(data)
    plot_comparison(data, warmup=args.warmup)


if __name__ == "__main__":
    main()
