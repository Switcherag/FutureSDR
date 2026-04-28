#!/usr/bin/env python3
"""Generate latency benchmark graphs from iox2-bench JSON output."""

import json
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker
import numpy as np

with open("bench_results.json") as f:
    data = json.load(f)

labels = [d["payload_label"] for d in data]
x = np.arange(len(labels))

median = [d["median_us"] for d in data]
mean = [d["mean_us"] for d in data]
p90 = [d["p90_us"] for d in data]
p99 = [d["p99_us"] for d in data]
min_v = [d["min_us"] for d in data]
max_v = [d["max_us"] for d in data]
payload_kb = [d["payload_bytes"] / 1024 for d in data]

fig, axes = plt.subplots(1, 3, figsize=(17, 5.5))
fig.suptitle(
    "iceoryx2 Shared Memory Latency — Ping-Pong Round-Trip (2000 iterations)",
    fontsize=13,
    fontweight="bold",
)

# ── Graph 1: Median + P99 bar chart ──
ax = axes[0]
w = 0.35
b1 = ax.bar(x - w / 2, median, w, label="Median", color="#2196F3", edgecolor="white")
b2 = ax.bar(x + w / 2, p99, w, label="P99", color="#F44336", edgecolor="white")
ax.set_xlabel("Payload Size")
ax.set_ylabel("Latency (us)")
ax.set_title("Median vs P99 Latency")
ax.set_xticks(x)
ax.set_xticklabels(labels, rotation=35, ha="right", fontsize=8)
ax.set_yscale("log")
ax.legend()
ax.grid(axis="y", alpha=0.3, which="both")
for bar in b1:
    ax.text(
        bar.get_x() + bar.get_width() / 2,
        bar.get_height() * 1.15,
        f"{bar.get_height():.1f}",
        ha="center",
        va="bottom",
        fontsize=7,
    )
for bar in b2:
    ax.text(
        bar.get_x() + bar.get_width() / 2,
        bar.get_height() * 1.15,
        f"{bar.get_height():.1f}",
        ha="center",
        va="bottom",
        fontsize=7,
    )

# ── Graph 2: Percentile spread (min / median / p90 / p99 / max) ──
ax = axes[1]
ax.fill_between(x, min_v, max_v, alpha=0.12, color="#9C27B0", label="Min–Max")
ax.fill_between(x, median, p99, alpha=0.25, color="#F44336", label="Median–P99")
ax.fill_between(x, median, p90, alpha=0.35, color="#FF9800", label="Median–P90")
ax.plot(x, median, "o-", color="#2196F3", linewidth=2, markersize=5, label="Median")
ax.plot(x, p99, "s--", color="#F44336", linewidth=1.2, markersize=4, label="P99")
ax.set_xlabel("Payload Size")
ax.set_ylabel("Latency (us)")
ax.set_title("Latency Distribution")
ax.set_xticks(x)
ax.set_xticklabels(labels, rotation=35, ha="right", fontsize=8)
ax.set_yscale("log")
ax.legend(fontsize=7, loc="upper left")
ax.grid(axis="y", alpha=0.3, which="both")

# ── Graph 3: Throughput derived from median latency ──
ax = axes[2]
# Throughput = payload_bytes / (median_latency / 2) in GB/s
# divide by 2 for one-way estimate
tp_gbps = [
    (d["payload_bytes"] / (d["median_us"] / 2 * 1e-6)) / (1024**3) for d in data
]
bars = ax.bar(x, tp_gbps, 0.55, color="#4CAF50", edgecolor="white")
ax.set_xlabel("Payload Size")
ax.set_ylabel("Throughput (GB/s)")
ax.set_title("Effective Throughput (from median RTT/2)")
ax.set_xticks(x)
ax.set_xticklabels(labels, rotation=35, ha="right", fontsize=8)
ax.grid(axis="y", alpha=0.3)
for bar in bars:
    ax.text(
        bar.get_x() + bar.get_width() / 2,
        bar.get_height() + max(tp_gbps) * 0.02,
        f"{bar.get_height():.2f}",
        ha="center",
        va="bottom",
        fontsize=8,
    )

plt.tight_layout()
plt.savefig("bench_graphs.png", dpi=150, bbox_inches="tight")
plt.savefig("bench_graphs.svg", bbox_inches="tight")
print("Saved bench_graphs.png and bench_graphs.svg")
