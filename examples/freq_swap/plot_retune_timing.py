#!/usr/bin/env python3
"""Visualize a `retune_timing` capture and read off SDR retune latency.

The capture contains:
  - the steady-state RX IQ (a tone, by assumption)
  - a high-amplitude marker pulse at the sample where the `mark` message
    was serviced by the in-flowgraph Marker block
  - a frequency-shift transient where the hardware actually retuned

Latency = time between the marker pulse and the start of the freq shift.
The plot shows |IQ| over time with both events annotated, and prints the
gap in samples + milliseconds.

Usage:
    python3 plot_retune_timing.py [iq_file] [meta_file] [output_png]

Defaults to ./retune_timing.cf32, ./retune_timing.meta.json, ./retune_timing.png.
"""

import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np


def load_meta(path: Path) -> dict:
    if path.exists():
        with path.open() as handle:
            return json.load(handle)
    sys.exit(f"missing {path} — run retune_timing first")


def load_iq(path: Path) -> np.ndarray:
    raw = np.fromfile(path, dtype=np.float32)
    if raw.size % 2:
        raw = raw[:-1]
    return raw.view(np.complex64)


def find_marker(iq: np.ndarray, mark_amp: float, mark_samples: int) -> int | None:
    """Return the sample index where the marker pulse starts, or None.

    The marker overwrites `mark_samples` consecutive samples with amplitude
    ~mark_amp. We look for the first sample whose |IQ| exceeds half the
    marker amplitude — that's the leading edge of the pulse.
    """
    threshold = mark_amp * 0.5
    above = np.abs(iq) > threshold
    if not above.any():
        return None
    return int(np.argmax(above))


def find_freq_shift(
    iq: np.ndarray,
    fs: float,
    marker_idx: int,
    marker_len: int,
    window_ms: float = 0.05,
) -> int | None:
    """Detect the moment the IF tone changes character after the marker.

    Strategy: compute short-window instantaneous frequency (via phase diff)
    over the region right after the marker, and locate the first sample
    whose freq differs significantly from the pre-marker baseline.

    Returns the sample index where the shift starts, or None if no clear
    transition is found.
    """
    # Baseline = mean inst. freq from a chunk BEFORE the marker.
    pre_lo = max(0, marker_idx - int(0.5 * fs * 1e-3))  # 0.5 ms before
    pre_hi = max(0, marker_idx - 1)
    if pre_hi - pre_lo < 64:
        return None
    pre_iq = iq[pre_lo:pre_hi]
    pre_phase_diff = np.angle(pre_iq[1:] * np.conj(pre_iq[:-1]))
    baseline_freq = np.mean(pre_phase_diff) * fs / (2 * np.pi)

    # Scan after the marker.
    scan_start = marker_idx + marker_len + 4  # skip marker artifacts
    scan_end = min(iq.size - 1, scan_start + int(window_ms * 1e-3 * fs * 200))
    if scan_end - scan_start < 64:
        return None
    win = max(16, int(fs * window_ms * 1e-3 / 4))  # ~window_ms/4 samples
    post_iq = iq[scan_start:scan_end]
    if post_iq.size < win + 2:
        return None

    # Rolling instantaneous freq via phase difference, smoothed by `win`.
    phase_diff = np.angle(post_iq[1:] * np.conj(post_iq[:-1]))
    kernel = np.ones(win) / win
    smoothed = np.convolve(phase_diff, kernel, mode="valid")
    smoothed_hz = smoothed * fs / (2 * np.pi)

    # First sample whose smoothed freq differs by more than 5% of fs.
    # (Use a generous threshold — the change can be small or large.)
    threshold = max(abs(baseline_freq) * 0.5, fs * 0.001)
    shifted = np.abs(smoothed_hz - baseline_freq) > threshold
    if not shifted.any():
        return None
    return scan_start + int(np.argmax(shifted))


def main():
    iq_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("retune_timing.cf32")
    meta_path = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("retune_timing.meta.json")
    out_path = Path(sys.argv[3]) if len(sys.argv) > 3 else Path("retune_timing.png")

    meta = load_meta(meta_path)
    fs = float(meta["sample_rate_hz"])
    mark_amp = float(meta["mark_amp"])
    mark_samples = int(meta["mark_samples"])
    start_freq = float(meta["start_freq_hz"])
    target_freq = float(meta["target_freq_hz"])

    iq = load_iq(iq_path)
    if iq.size == 0:
        sys.exit(f"{iq_path} is empty")
    t_axis = np.arange(iq.size) / fs * 1e3  # ms

    marker_idx = find_marker(iq, mark_amp, mark_samples)
    if marker_idx is None:
        print("WARN: no marker pulse detected. Try increasing --mark-amp.", file=sys.stderr)
    shift_idx = (
        find_freq_shift(iq, fs, marker_idx, mark_samples)
        if marker_idx is not None
        else None
    )

    fig, (ax_mag, ax_iq) = plt.subplots(
        2, 1, figsize=(13, 7), sharex=True, height_ratios=[1, 2]
    )

    mag = np.abs(iq)
    ax_mag.plot(t_axis, mag, color="#444", lw=0.6)
    ax_mag.set_ylabel("|IQ|")
    ax_mag.set_title(
        f"retune_timing — start {start_freq/1e6:.6f} MHz → target {target_freq/1e6:.6f} MHz "
        f"(Δ = {(target_freq - start_freq)/1e3:+.3f} kHz)"
    )

    ax_iq.plot(t_axis, iq.real, color="#1f77b4", lw=0.6, label="I")
    ax_iq.plot(t_axis, iq.imag, color="#ff7f0e", lw=0.6, alpha=0.7, label="Q")
    ax_iq.set_xlabel("time (ms)")
    ax_iq.set_ylabel("IQ amplitude")
    ax_iq.set_ylim(-1.5, 1.5)  # ignore the marker spike in IQ view
    ax_iq.legend(loc="upper right")

    if marker_idx is not None:
        t_mark = marker_idx / fs * 1e3
        for ax in (ax_mag, ax_iq):
            ax.axvline(t_mark, color="green", lw=1.2, ls="--", alpha=0.9,
                       label=f"marker @ {t_mark:.3f} ms")
        ax_mag.text(t_mark, mag.max() * 0.9, " marker", color="green", fontsize=9)
        print(f"marker pulse at sample {marker_idx}  (t = {t_mark:.3f} ms)")

    if shift_idx is not None:
        t_shift = shift_idx / fs * 1e3
        for ax in (ax_mag, ax_iq):
            ax.axvline(t_shift, color="red", lw=1.2, ls="--", alpha=0.9,
                       label=f"freq shift @ {t_shift:.3f} ms")
        print(f"freq shift   at sample {shift_idx}  (t = {t_shift:.3f} ms)")
        if marker_idx is not None:
            gap_samples = shift_idx - marker_idx
            gap_ms = gap_samples / fs * 1e3
            print(f"RETUNE LATENCY: {gap_samples} samples = {gap_ms*1e3:.1f} µs ({gap_ms:.3f} ms)")
            ax_mag.annotate(
                f"Δ = {gap_ms*1e3:.1f} µs",
                xy=((t_mark + t_shift) / 2, mag.max() * 0.6),
                ha="center",
                fontsize=11,
                color="purple",
                bbox=dict(boxstyle="round,pad=0.3", fc="#fff3", ec="purple"),
            )
    else:
        print("WARN: no clear freq shift detected. Try a smaller Δfreq (in-band) "
              "or a longer --total-ms to see the transition.", file=sys.stderr)

    for ax in (ax_mag, ax_iq):
        ax.grid(True, alpha=0.3)
        # Dedupe legend entries
        h, l = ax.get_legend_handles_labels()
        if h:
            seen = set()
            uniq = [(hh, ll) for hh, ll in zip(h, l) if not (ll in seen or seen.add(ll))]
            ax.legend(*zip(*uniq), loc="upper right", fontsize=8)

    fig.tight_layout()
    fig.savefig(out_path, dpi=130, bbox_inches="tight")
    print(f"wrote {out_path}")
    plt.show()


if __name__ == "__main__":
    main()
