#!/usr/bin/env python3
"""Plot PER vs RX gain per PHY, from data/per_sweep_summary.csv."""
import csv
from pathlib import Path

import matplotlib.pyplot as plt

SUMMARY = Path(__file__).with_name("data") / "per_sweep_summary.csv"
OUT_PATH = Path(__file__).with_name("per_vs_gain.png")


def main():
    if not SUMMARY.exists():
        raise SystemExit(
            f"missing {SUMMARY} — run run_per_sweep.py first"
        )

    series = {}  # phy → list of (gain, per, received, expected)
    with SUMMARY.open() as f:
        for row in csv.DictReader(f):
            series.setdefault(row["phy"], []).append((
                float(row["gain_db"]),
                float(row["per"]),
                int(row["received"]),
                int(row["expected"]),
            ))

    fig, ax = plt.subplots(figsize=(9, 5.5))
    for phy, rows in sorted(series.items()):
        rows.sort(key=lambda r: r[0])
        gains = [r[0] for r in rows]
        pers = [r[1] for r in rows]
        ax.plot(gains, pers, marker="o", linewidth=2, label=phy)
        for g, p, recv, exp in rows:
            ax.annotate(f"{recv}/{exp}", xy=(g, p),
                        xytext=(4, 4), textcoords="offset points",
                        fontsize=7)

    ax.set_xlabel("RX gain (dB)")
    ax.set_ylabel("PER (1 − received / expected)")
    ax.set_title("Per-PHY PER vs RX gain")
    ax.set_ylim(-0.02, 1.02)
    ax.grid(True, alpha=0.3)
    ax.legend()
    fig.tight_layout()
    fig.savefig(OUT_PATH, dpi=130)
    print(f"wrote {OUT_PATH}")


if __name__ == "__main__":
    main()
