#!/usr/bin/env python3
"""Plot time-domain I/Q traces for a `freq_swap` IQ capture.

Usage:
    python3 plot_freq_swap_iq.py [iq_file] [meta_file] [output_png]

Defaults to ./freq_swap.cf32, ./freq_swap.meta.json and ./freq_swap_iq.png.
"""

import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

DEFAULT_SAMPLE_RATE_HZ = 4e6
DEFAULT_MAX_POINTS = 250_000


def load_meta(path: Path) -> dict:
    if path.exists():
        with path.open() as handle:
            return json.load(handle)
    print(f"WARN: {path} not found, using defaults", file=sys.stderr)
    return {
        "sample_rate_hz": DEFAULT_SAMPLE_RATE_HZ,
        "tx_retune_t_s": None,
        "rx_retune_t_s": None,
    }


def load_iq(path: Path) -> np.ndarray:
    raw = np.fromfile(path, dtype=np.float32)
    if raw.size % 2:
        raw = raw[:-1]
    return raw.view(np.complex64)


def decimate_for_plot(iq: np.ndarray, max_points: int) -> tuple[np.ndarray, int]:
    step = max(1, int(np.ceil(iq.size / max_points)))
    return iq[::step], step


def add_event_markers(ax, tx_t, rx_t):
    if tx_t is not None:
        ax.axvline(tx_t, color="red", lw=1.0, ls="--", alpha=0.85, label=f"TX retune @ {tx_t:.6f}s")
    if rx_t is not None:
        ax.axvline(rx_t, color="orange", lw=1.0, ls="--", alpha=0.85, label=f"RX retune @ {rx_t:.6f}s")


def main():
    iq_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("freq_swap.cf32")
    meta_path = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("freq_swap.meta.json")
    out_path = Path(sys.argv[3]) if len(sys.argv) > 3 else Path("freq_swap_iq.png")

    meta = load_meta(meta_path)
    fs = float(meta.get("sample_rate_hz", DEFAULT_SAMPLE_RATE_HZ))
    tx_t = meta.get("tx_retune_t_s")
    rx_t = meta.get("rx_retune_t_s")

    iq = load_iq(iq_path)
    if iq.size == 0:
        sys.exit(f"empty IQ file: {iq_path}")

    iq_plot, stride = decimate_for_plot(iq, DEFAULT_MAX_POINTS)
    time_s = np.arange(iq_plot.size, dtype=np.float64) * (stride / fs)
    magnitude = np.abs(iq_plot)

    duration_s = iq.size / fs
    effective_rate = fs / stride
    print(
        f"Loaded {iq.size} complex samples ({duration_s:.6f} s) @ {fs/1e6:.3f} MSPS; "
        f"plotting every {stride} sample(s) ({effective_rate/1e3:.1f} kS/s view)"
    )

    fig, axes = plt.subplots(2, 1, figsize=(12, 7), sharex=True, constrained_layout=True)
    ax_iq, ax_mag = axes

    ax_iq.plot(time_s, iq_plot.real, lw=0.7, color="tab:blue", label="I")
    ax_iq.plot(time_s, iq_plot.imag, lw=0.7, color="tab:orange", label="Q")
    add_event_markers(ax_iq, tx_t, rx_t)
    ax_iq.set_ylabel("Amplitude")
    ax_iq.set_title(f"freq_swap IQ vs time ({iq_path.name})")
    ax_iq.grid(True, alpha=0.3)
    ax_iq.legend(loc="upper right")

    ax_mag.plot(time_s, magnitude, lw=0.8, color="tab:green", label="|IQ|")
    add_event_markers(ax_mag, tx_t, rx_t)
    ax_mag.set_xlabel("Time [s]")
    ax_mag.set_ylabel("Magnitude")
    ax_mag.grid(True, alpha=0.3)
    ax_mag.legend(loc="upper right")

    fig.savefig(out_path, dpi=140)
    print(f"Saved figure -> {out_path}")
    plt.show()


if __name__ == "__main__":
    main()