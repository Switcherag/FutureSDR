#!/usr/bin/env python3
"""3D surface of bladeRF power over bandwidth × frequency at max gain.

All surfaces are overlaid in a single 3D axes:
  * TX: one surface per waveform (constant / sine / noise)
  * RX: a single surface in one colour (no waveform axis)

Reuses the parsing and alignment from plot_consumption.py (offsets.json
calibration). The figure is also dumped as a pickle so it can be reopened and
rotated later (see show_pickle.py).

    python3 surface_graph.py            # show
    python3 surface_graph.py --no-show  # save PNG + pickle only
"""
import argparse
import pickle
from pathlib import Path

import numpy as np
import matplotlib.pyplot as plt
from matplotlib.patches import Patch
from mpl_toolkits.mplot3d import Axes3D  # noqa: F401  (registers 3d projection)

import plot_consumption as P

HERE = Path(__file__).parent
COLORS = {"constant": "tab:blue", "sine": "tab:orange", "noise": "tab:green"}
RX_COLOR = "tab:red"


def load_dataset(label):
    """Return (stats, kind) for a dataset, or None if its files are missing."""
    spec = P.DATASETS.get(label)
    if spec is None:
        return None
    ppath, lpath = HERE / spec["power"], HERE / spec["log"]
    if not ppath.exists() or not lpath.exists():
        print(f"[{label}] missing {[p.name for p in (ppath, lpath) if not p.exists()]}")
        return None
    power, log = P.load_power(ppath), P.load_log(lpath)
    cal = P.load_offsets().get(label, {})
    if isinstance(cal, dict):
        off, settle, window = cal.get("offset"), cal.get("settle", 1.0), cal.get("window")
    else:
        off, settle, window = cal, 1.0, None
    if off is None:
        off = P.auto_offset(log, power["t"], power["w"])[0]
    return P.window_stats(power, log, off, settle, window), log["kind"]


def grid(stats, gmax, freqs, bws, sig):
    """Z[freq, bw] of mean power at the given gain / waveform."""
    def pw_at(freq, bw):
        for r in stats:
            if (r["gain_db"] == gmax and r["freq_hz"] == freq
                    and r["bw_hz"] == bw and r["signal"] == sig):
                return r["power_mean_w"]
        return np.nan
    return np.array([[pw_at(f, b) for b in bws] for f in freqs])


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--no-show", action="store_true", help="save PNG + pickle only")
    args = ap.parse_args()

    panels = []
    for label in ("blade_tx", "blade_rx"):
        d = load_dataset(label)
        if d is not None:
            panels.append((label, d))
    if not panels:
        print("No blade datasets found.")
        return

    # Shared bandwidth/frequency grid (same sweep for TX and RX).
    ref_stats = panels[0][1][0]
    freqs = sorted({r["freq_hz"] for r in ref_stats})
    bws = sorted({r["bw_hz"] for r in ref_stats})
    bi, fi = np.arange(len(bws)), np.arange(len(freqs))
    BW, FREQ = np.meshgrid(bi, fi)

    fig = plt.figure(figsize=(11, 8))
    ax = fig.add_subplot(111, projection="3d")
    handles = []
    for label, (stats, kind) in panels:
        gmax = max(r["gain_db"] for r in stats)
        if kind == "tx":
            for sig in P.SIGNAL_ORDER:
                if not any(r["signal"] == sig for r in stats):
                    continue
                Z = grid(stats, gmax, freqs, bws, sig)
                ax.plot_surface(BW, FREQ, Z, color=COLORS[sig], alpha=0.5,
                                edgecolor="k", linewidth=0.3)
                handles.append(Patch(color=COLORS[sig], label=f"tx {sig}"))
        else:
            Z = grid(stats, gmax, freqs, bws, "")
            ax.plot_surface(BW, FREQ, Z, color=RX_COLOR, alpha=0.5,
                            edgecolor="k", linewidth=0.3)
            handles.append(Patch(color=RX_COLOR, label="rx"))
        print(f"[{label}] {kind} surface @ {gmax:.0f} dB gain")

    ax.set_xticks(bi)
    ax.set_xticklabels([f"{b / 1e6:.0f}" for b in bws])
    ax.set_yticks(fi)
    ax.set_yticklabels([f"{f / 1e6:.0f}" for f in freqs])
    ax.set_xlabel("bandwidth (MHz)")
    ax.set_ylabel("frequency (MHz)")
    ax.set_zlabel("mean power (W)")
    ax.set_title("bladeRF power surface  —  bandwidth × frequency @ max gain")
    ax.legend(handles=handles, loc="upper left")
    ax.view_init(elev=22, azim=-60)

    fig.savefig(HERE / "consumption_surface.png", dpi=120, bbox_inches="tight")
    with open(HERE / "consumption_surface.fig.pickle", "wb") as f:
        pickle.dump(fig, f)
    print("wrote consumption_surface.png + consumption_surface.fig.pickle")
    if not args.no_show:
        plt.show()


if __name__ == "__main__":
    main()
