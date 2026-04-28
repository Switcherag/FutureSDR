#!/usr/bin/env python3
"""
Probability density histogram of frequency change latency — 4 methods.

Reads:
  bladerf_bench_c/bladerf_random_freq_bench.csv  (set_frequency, schedule_retune)
  sdr_random_freq_bench.csv                      (soapy, bladerf1)

CSV format: method,iteration,from_hz,to_hz,time_us
"""

import csv
import numpy as np
import matplotlib.pyplot as plt
from pathlib import Path
from collections import defaultdict

SCRIPT_DIR = Path(__file__).parent
C_CSV = SCRIPT_DIR / "bladerf_bench_c" / "bladerf_random_freq_bench.csv"
RUST_CSV = SCRIPT_DIR / "sdr_random_freq_bench.csv"

METHODS = [
    ("set_frequency",   "libbladeRF set_frequency",  "#2196F3"),
    ("schedule_retune", "libbladeRF quick_tune",      "#4CAF50"),
    ("soapy",           "seify / SoapySDR",           "#FF9800"),
    ("bladerf1",        "seify / bladerf1 (native)",  "#9C27B0"),
]


def load_csv(path):
    data = defaultdict(list)
    if not path.exists():
        return data
    with open(path) as f:
        for row in csv.DictReader(f):
            data[row["method"]].append(float(row["time_us"]))
    return data


c_data = load_csv(C_CSV)
rust_data = load_csv(RUST_CSV)

all_data = {}
for k, _, _ in METHODS:
    if k in c_data:
        all_data[k] = np.array(c_data[k])
    elif k in rust_data:
        all_data[k] = np.array(rust_data[k])

available = [(k, l, c) for k, l, c in METHODS if k in all_data]
if not available:
    print("ERROR: no data. Run benchmarks first.")
    exit(1)

# ── Print summary ──
print(f"\n{'Method':<30s} {'N':>6s} {'Mean':>8s} {'Median':>8s} {'P95':>8s} {'P99':>8s} {'Max':>8s} µs")
print("─" * 82)
for k, l, _ in available:
    a = all_data[k]
    print(f"{l:<30s} {len(a):>6d} {np.mean(a):>8.0f} {np.median(a):>8.0f} "
          f"{np.percentile(a,95):>8.0f} {np.percentile(a,99):>8.0f} {np.max(a):>8.0f}")
print()

# ── Plot ──
fig, ax = plt.subplots(figsize=(12, 6))

# Clip at p99 of slowest method for readability
x_max = max(np.percentile(all_data[k], 99.5) for k, _, _ in available)

# Shared bins
bins = np.linspace(0, x_max, 80)

for k, label, color in available:
    a = all_data[k]
    mu = np.mean(a)
    ax.hist(a, bins=bins, density=True, alpha=0.55, color=color, edgecolor="white",
            linewidth=0.4, label=f"{label}  (mean={mu:.0f} µs, n={len(a)})")
    ax.axvline(mu, color=color, linewidth=1.8, linestyle="--", alpha=0.9)

ax.set_xlabel("Latency (µs)", fontsize=12)
ax.set_ylabel("Probability density", fontsize=12)
ax.set_title("Frequency Change Latency — bladeRF 2.0 (70 MHz – 5.9 GHz)\n"
             "Random hops, 1000 iterations per method",
             fontsize=14, fontweight="bold")
ax.legend(fontsize=10, loc="upper right")
ax.set_xlim(0, x_max)
ax.grid(axis="y", alpha=0.3)

plt.tight_layout()
out = SCRIPT_DIR / "random_freq_bench_histogram.png"
plt.savefig(out, dpi=150, bbox_inches="tight")
print(f"Saved to {out}")
plt.close()
