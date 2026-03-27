#!/usr/bin/env python3
"""Plot SDR tuning grid benchmarks as heatmaps.

Input CSV format (long):
    param,from,to,run,time_ms
Where:
    param in {rate, freq}
    from/to are integer MHz values
    run is integer index (e.g. 0..4)
    time_ms is latency in milliseconds (or -1 on error)

This script expects three datasets:
- bladeRF via FutureSDR/Soapy (Rust grid bench)
- bladeRF native C (libbladeRF direct)
- ADALM Pluto via FutureSDR/Soapy (Rust grid bench)

It outputs:
- rate_grid_comparison.png
- freq_grid_comparison.png
"""

from __future__ import annotations

import argparse
import csv
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Tuple

import matplotlib.pyplot as plt
import numpy as np

RATE_AXIS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
FREQ_AXIS = [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000]


@dataclass
class Dataset:
    label: str
    path: Path


def load_grid(csv_path: Path, param: str, axis: List[int]) -> np.ndarray:
    """Return NxN matrix of mean time_ms for (from -> to) transitions.

    Invalid rows (time_ms < 0) are ignored. Missing cells become NaN.
    """
    index = {v: i for i, v in enumerate(axis)}
    n = len(axis)
    buckets: Dict[Tuple[int, int], List[float]] = {}

    with csv_path.open("r", newline="") as f:
        reader = csv.DictReader(f)
        expected = {"param", "from", "to", "run", "time_ms"}
        if not reader.fieldnames or set(reader.fieldnames) != expected:
            raise ValueError(
                f"{csv_path}: invalid columns {reader.fieldnames}, expected {sorted(expected)}"
            )

        for row in reader:
            if row["param"] != param:
                continue
            frm = int(row["from"])
            to = int(row["to"])
            t = float(row["time_ms"])
            if frm not in index or to not in index:
                continue
            if t < 0:
                continue
            buckets.setdefault((frm, to), []).append(t)

    grid = np.full((n, n), np.nan, dtype=float)
    for frm in axis:
        for to in axis:
            values = buckets.get((frm, to), [])
            if values:
                grid[index[frm], index[to]] = float(np.mean(values))

    return grid


def global_limits(grids: List[np.ndarray]) -> Tuple[float, float]:
    vals = np.concatenate([g[np.isfinite(g)] for g in grids if np.isfinite(g).any()])
    if vals.size == 0:
        return 0.0, 1.0
    return float(np.min(vals)), float(np.max(vals))


def plot_param(
    datasets: List[Dataset],
    param: str,
    axis: List[int],
    out_file: Path,
    title: str,
) -> None:
    grids = [load_grid(d.path, param, axis) for d in datasets]
    vmin, vmax = global_limits(grids)

    fig, axes = plt.subplots(1, len(datasets), figsize=(6.5 * len(datasets), 5.8), dpi=120)
    if len(datasets) == 1:
        axes = [axes]

    for ax, ds, grid in zip(axes, datasets, grids):
        im = ax.imshow(grid, origin="lower", aspect="equal", cmap="viridis", vmin=vmin, vmax=vmax)
        ax.set_title(ds.label, fontsize=11)
        ax.set_xlabel("to")
        ax.set_ylabel("from")

        ticks = np.arange(len(axis))
        tick_labels = [str(v) for v in axis]
        ax.set_xticks(ticks)
        ax.set_yticks(ticks)
        ax.set_xticklabels(tick_labels, rotation=45, ha="right", fontsize=8)
        ax.set_yticklabels(tick_labels, fontsize=8)

        # Light grid overlay for readability of matrix cells.
        ax.set_xticks(np.arange(-0.5, len(axis), 1), minor=True)
        ax.set_yticks(np.arange(-0.5, len(axis), 1), minor=True)
        ax.grid(which="minor", color="white", linewidth=0.25, alpha=0.3)

    cbar = fig.colorbar(im, ax=axes, fraction=0.03, pad=0.03)
    cbar.set_label("Latency (ms)")
    fig.suptitle(title, fontsize=13)
    fig.tight_layout()
    fig.savefig(out_file, bbox_inches="tight")
    plt.close(fig)


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Plot SDR tuning grids for three SDR paths")
    p.add_argument(
        "--bladerf",
        default="sdr_grid_bench_bladerf.csv",
        help="Rust/Soapy bladeRF grid CSV",
    )
    p.add_argument(
        "--bladerf-c",
        dest="bladerf_c",
        default="examples/sdr_tuning/bladerf_bench_c/bladerf_native_grid.csv",
        help="Native C bladeRF grid CSV",
    )
    p.add_argument(
        "--pluto",
        default="sdr_grid_bench_pluto.csv",
        help="Rust/Soapy Pluto grid CSV",
    )
    p.add_argument(
        "--out-dir",
        default="tmp",
        help="Output directory for generated figures",
    )
    return p.parse_args()


def main() -> None:
    args = parse_args()
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    datasets = [
        Dataset("bladeRF (FutureSDR/Soapy)", Path(args.bladerf)),
        Dataset("bladeRF native C (libbladeRF)", Path(args.bladerf_c)),
        Dataset("ADALM Pluto (FutureSDR/Soapy)", Path(args.pluto)),
    ]

    missing = [str(d.path) for d in datasets if not d.path.exists()]
    if missing:
        raise FileNotFoundError(
            "Missing CSV files:\n  - " + "\n  - ".join(missing)
        )

    plot_param(
        datasets,
        param="rate",
        axis=RATE_AXIS,
        out_file=out_dir / "rate_grid_comparison.png",
        title="Sample Rate Transition Grid (1..10 MHz, mean of runs)",
    )
    plot_param(
        datasets,
        param="freq",
        axis=FREQ_AXIS,
        out_file=out_dir / "freq_grid_comparison.png",
        title="Frequency Transition Grid (100..1000 MHz, mean of runs)",
    )

    print(f"Saved: {out_dir / 'rate_grid_comparison.png'}")
    print(f"Saved: {out_dir / 'freq_grid_comparison.png'}")


if __name__ == "__main__":
    main()
