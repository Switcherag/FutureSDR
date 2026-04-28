#!/usr/bin/env python3
"""Plot swap benchmark results from cross_fg_bench.csv and/or swap_bench.csv"""

import csv
import os
import sys
from collections import defaultdict

try:
    import matplotlib.pyplot as plt
    import matplotlib.ticker as ticker
    import numpy as np
except ImportError:
    print("pip install matplotlib numpy")
    sys.exit(1)

SCRIPT_DIR = os.path.dirname(__file__) or "."


def load_csv(path):
    """Load a bench CSV -> dict[target] = [time_ms, ...]"""
    data = defaultdict(list)
    with open(path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            data[row["target"]].append(float(row["time_ms"]))
    return dict(data)


def make_sample_data():
    """Sample data for demonstration when no CSV is available."""
    rng = np.random.default_rng(42)
    return {
        "wlan_rx.toml":   (rng.normal(85, 12, 30)).tolist(),
        "zigbee_rx.toml": (rng.normal(45, 8, 30)).tolist(),
        "discard.toml":   (rng.normal(15, 3, 30)).tolist(),
    }


def plot_single(data, title, output_path, sample=False):
    """3-panel plot for one benchmark."""
    targets = list(data.keys())
    n_targets = len(targets)

    fig, axes = plt.subplots(1, 3, figsize=(15, 5), gridspec_kw={"width_ratios": [3, 1, 1]})
    fig.suptitle(
        title + (" (sample data)" if sample else ""),
        fontsize=14, fontweight="bold",
    )

    colors = plt.cm.Set2(np.linspace(0, 1, max(n_targets, 3)))

    # --- Panel 1: time series ---
    ax = axes[0]
    for i, target in enumerate(targets):
        times = data[target]
        x = list(range(1, len(times) + 1))
        ax.plot(x, times, "o-", color=colors[i], label=target, markersize=4, linewidth=1.2)
    ax.set_xlabel("Swap #")
    ax.set_ylabel("Swap time (ms)")
    ax.set_title("Per-swap latency")
    ax.legend(fontsize=9)
    ax.grid(True, alpha=0.3)
    ax.xaxis.set_major_locator(ticker.MaxNLocator(integer=True))

    # --- Panel 2: box plot ---
    ax = axes[1]
    bp = ax.boxplot(
        [data[t] for t in targets],
        tick_labels=[t.replace(".toml", "") for t in targets],
        patch_artist=True,
    )
    for patch, c in zip(bp["boxes"], colors):
        patch.set_facecolor(c)
        patch.set_alpha(0.7)
    ax.set_ylabel("Swap time (ms)")
    ax.set_title("Distribution")
    ax.grid(True, alpha=0.3, axis="y")
    plt.setp(ax.get_xticklabels(), rotation=30, ha="right", fontsize=9)

    # --- Panel 3: summary bar ---
    ax = axes[2]
    means = [np.mean(data[t]) for t in targets]
    stds = [np.std(data[t]) for t in targets]
    short = [t.replace(".toml", "") for t in targets]
    bars = ax.bar(short, means, yerr=stds, color=colors[:n_targets], alpha=0.8, capsize=4)
    for bar, m in zip(bars, means):
        ax.text(bar.get_x() + bar.get_width() / 2, bar.get_height() + 1,
                f"{m:.1f}", ha="center", va="bottom", fontsize=9)
    ax.set_ylabel("Mean swap time (ms)")
    ax.set_title("Average +/- std")
    ax.grid(True, alpha=0.3, axis="y")
    plt.setp(ax.get_xticklabels(), rotation=30, ha="right", fontsize=9)

    plt.tight_layout()
    fig.savefig(output_path, dpi=150, bbox_inches="tight")
    print(f"Saved: {output_path}")


def plot_comparison(data_fg, data_radio, output_path):
    """Side-by-side comparison of FG-only vs Radio swap times."""
    # Collect common targets
    all_targets = list(dict.fromkeys(list(data_fg.keys()) + list(data_radio.keys())))

    fig, axes = plt.subplots(1, 2, figsize=(14, 5))
    fig.suptitle("Cross-Flowgraph vs Radio Swap Benchmark", fontsize=14, fontweight="bold")

    colors = plt.cm.Set2(np.linspace(0, 1, max(len(all_targets), 3)))

    # Panel 1: box plot comparison
    ax = axes[0]
    positions = []
    box_data = []
    box_colors = []
    labels = []
    pos = 0
    for i, target in enumerate(all_targets):
        short = target.replace(".toml", "")
        if target in data_fg:
            box_data.append(data_fg[target])
            positions.append(pos)
            box_colors.append(colors[i])
            labels.append(f"{short}\n(FG)")
            pos += 1
        if target in data_radio:
            box_data.append(data_radio[target])
            positions.append(pos)
            box_colors.append(colors[i])
            labels.append(f"{short}\n(Radio)")
            pos += 1
        pos += 0.5  # gap between targets

    bp = ax.boxplot(box_data, positions=positions, patch_artist=True, widths=0.6)
    for patch, c in zip(bp["boxes"], box_colors):
        patch.set_facecolor(c)
        patch.set_alpha(0.7)
    ax.set_xticks(positions)
    ax.set_xticklabels(labels, fontsize=8)
    ax.set_ylabel("Swap time (ms)")
    ax.set_title("Distribution comparison")
    ax.grid(True, alpha=0.3, axis="y")

    # Panel 2: grouped bar chart
    ax = axes[1]
    x = np.arange(len(all_targets))
    width = 0.35
    fg_means = [np.mean(data_fg.get(t, [0])) for t in all_targets]
    fg_stds = [np.std(data_fg.get(t, [0])) for t in all_targets]
    radio_means = [np.mean(data_radio.get(t, [0])) for t in all_targets]
    radio_stds = [np.std(data_radio.get(t, [0])) for t in all_targets]
    short_names = [t.replace(".toml", "") for t in all_targets]

    bars1 = ax.bar(x - width/2, fg_means, width, yerr=fg_stds,
                   label="FG only", color="#66c2a5", alpha=0.8, capsize=3)
    bars2 = ax.bar(x + width/2, radio_means, width, yerr=radio_stds,
                   label="Radio + FG", color="#fc8d62", alpha=0.8, capsize=3)

    for bar, m in zip(bars1, fg_means):
        if m > 0:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 0.5,
                    f"{m:.1f}", ha="center", va="bottom", fontsize=8)
    for bar, m in zip(bars2, radio_means):
        if m > 0:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 0.5,
                    f"{m:.1f}", ha="center", va="bottom", fontsize=8)

    ax.set_xticks(x)
    ax.set_xticklabels(short_names, rotation=30, ha="right", fontsize=9)
    ax.set_ylabel("Mean swap time (ms)")
    ax.set_title("Average +/- std")
    ax.legend(fontsize=9)
    ax.grid(True, alpha=0.3, axis="y")

    plt.tight_layout()
    fig.savefig(output_path, dpi=150, bbox_inches="tight")
    print(f"Saved: {output_path}")


if __name__ == "__main__":
    cross_fg_csv = os.path.join(SCRIPT_DIR, "cross_fg_bench.csv")
    radio_csv = os.path.join(SCRIPT_DIR, "swap_bench.csv")

    has_fg = os.path.exists(cross_fg_csv)
    has_radio = os.path.exists(radio_csv)

    if has_fg:
        print(f"Loading {cross_fg_csv}")
        data_fg = load_csv(cross_fg_csv)
        out = os.path.join(SCRIPT_DIR, "cross_fg_bench.png")
        plot_single(data_fg, "Cross-Flowgraph Swap Benchmark (no SDR)", out)
    else:
        print(f"No {cross_fg_csv} found.")

    if has_radio:
        print(f"Loading {radio_csv}")
        data_radio = load_csv(radio_csv)
        out = os.path.join(SCRIPT_DIR, "swap_bench.png")
        plot_single(data_radio, "Radio Cross-Flowgraph Swap Benchmark", out)
    else:
        print(f"No {radio_csv} found.")

    if has_fg and has_radio:
        out = os.path.join(SCRIPT_DIR, "bench_comparison.png")
        plot_comparison(data_fg, data_radio, out)
    elif not has_fg and not has_radio:
        print("No CSV files found. Using sample data for demonstration.")
        data = make_sample_data()
        out = os.path.join(SCRIPT_DIR, "swap_bench.png")
        plot_single(data, "Cross-Flowgraph Swap Benchmark", out, sample=True)

    plt.show()
