#!/usr/bin/env python3
"""
Plot 3 frequency grids side by side:
  1. Native C set_frequency (bladerf_native_grid.csv or bladerf_quick_tune_bench.csv)
  2. Native C schedule_retune / quick_tune (bladerf_quick_tune_bench.csv)
  3. Rust/SoapySDR freq change (bladerf_soapy_grid.csv)
"""

import csv
import numpy as np
import matplotlib.pyplot as plt
from pathlib import Path

BENCH_DIR = Path(__file__).parent / "bladerf_bench_c"
FREQS = [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000]
N = len(FREQS)


def load_native_grid(path):
    """Load freq grid from bladerf_native_grid.csv (param,from,to,run,time_ms)"""
    grid = np.full((N, N), np.nan)
    counts = np.zeros((N, N))
    with open(path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            if row["param"] != "freq":
                continue
            fi = FREQS.index(int(row["from"]))
            ti = FREQS.index(int(row["to"]))
            t = float(row["time_ms"])
            if t < 0:
                continue
            if np.isnan(grid[fi][ti]):
                grid[fi][ti] = 0
            grid[fi][ti] += t
            counts[fi][ti] += 1
    mask = counts > 0
    grid[mask] /= counts[mask]
    return grid


def load_quick_tune_grid(path, method):
    """Load freq grid from bladerf_quick_tune_bench.csv (method,test,from,to,run,time_ms)"""
    grid = np.full((N, N), np.nan)
    counts = np.zeros((N, N))
    with open(path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            if row["method"] != method or row["test"] != "grid":
                continue
            fi = FREQS.index(int(row["from"]))
            ti = FREQS.index(int(row["to"]))
            t = float(row["time_ms"])
            if t < 0:
                continue
            if np.isnan(grid[fi][ti]):
                grid[fi][ti] = 0
            grid[fi][ti] += t
            counts[fi][ti] += 1
    mask = counts > 0
    grid[mask] /= counts[mask]
    return grid


def load_soapy_grid(path):
    """Load freq grid from bladerf_soapy_grid.csv (same format as native)"""
    return load_native_grid(path)


# Load data
native_grid_path = BENCH_DIR / "bladerf_native_grid.csv"
quick_tune_path = BENCH_DIR / "bladerf_quick_tune_bench.csv"
soapy_grid_path = BENCH_DIR / "bladerf_soapy_grid.csv"

# Use quick_tune_bench CSV for set_frequency grid (freshest data)
native_freq = load_quick_tune_grid(quick_tune_path, "set_frequency")
quick_tune_freq = load_quick_tune_grid(quick_tune_path, "schedule_retune")
soapy_freq = load_soapy_grid(soapy_grid_path)

# Global colorscale: use the max across all 3 grids
vmin = 0
vmax = max(np.nanmax(native_freq), np.nanmax(soapy_freq), np.nanmax(quick_tune_freq))

labels = [f"{f}" for f in FREQS]

fig, axes = plt.subplots(1, 3, figsize=(22, 7))

titles = [
    "Native C: set_frequency",
    "Native C: schedule_retune\n(quick_tune)",
    "Rust/SoapySDR: set_frequency",
]
grids = [native_freq, quick_tune_freq, soapy_freq]

for ax, title, grid in zip(axes, titles, grids):
    im = ax.imshow(grid, cmap="RdYlGn_r", vmin=vmin, vmax=vmax,
                   interpolation="nearest", aspect="equal")

    # Annotate cells
    for i in range(N):
        for j in range(N):
            val = grid[i][j]
            if np.isnan(val):
                ax.text(j, i, "-", ha="center", va="center", fontsize=7, color="gray")
            else:
                color = "white" if val > vmax * 0.6 else "black"
                ax.text(j, i, f"{val:.1f}", ha="center", va="center",
                        fontsize=7, fontweight="bold", color=color)

    ax.set_xticks(range(N))
    ax.set_yticks(range(N))
    ax.set_xticklabels(labels, fontsize=8)
    ax.set_yticklabels(labels, fontsize=8)
    ax.set_xlabel("To frequency (MHz)", fontsize=10)
    ax.set_ylabel("From frequency (MHz)", fontsize=10)
    ax.set_title(title, fontsize=12, fontweight="bold")

# Shared colorbar
cbar = fig.colorbar(im, ax=axes, shrink=0.8, pad=0.02)
cbar.set_label("Latency (ms)", fontsize=11)

fig.suptitle("Frequency Change Latency — 10×10 Grid (bladeRF 2.0, 4 MSPS)",
             fontsize=14, fontweight="bold", y=0.98)
plt.tight_layout(rect=[0, 0, 0.92, 0.95])

out = Path(__file__).parent / "freq_grid_comparison.png"
plt.savefig(out, dpi=150, bbox_inches="tight")
print(f"Saved to {out}")
plt.close()
