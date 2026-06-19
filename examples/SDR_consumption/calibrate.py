#!/usr/bin/env python3
"""Interactively calibrate the time offset and measurement window of an SDR
power-consumption capture, so per-config averages exclude reconfiguration
transients.

Opens one power-meter CSV, overlays a periodic config grid (period = --dwell)
and lets you drag three sliders:

  * offset  — align the grid to the power trace
  * settle  — skip this long at each config's start (front / settle transient)
  * window  — measurement length after settle (shrink to drop a tail transient,
              e.g. a device-rebuild gap at the window boundary)

The shaded orange bands are exactly the samples that would be averaged; the red
plateaus are their means. Tune until the bands sit on the flat parts and the
"band std" readout is minimised, then press Accept. The result is written to
calibration.json (and, with --label, the full {offset, settle, window} is stored
in offsets.json for plot_consumption.py).

    python3 calibrate.py csv/pluto_tx.csv --dwell 10 --label tx_pluto

Then plot:
    python3 plot_consumption.py --only tx_pluto
"""
import argparse
import json
from pathlib import Path

import numpy as np
import matplotlib.pyplot as plt
from matplotlib.widgets import Slider, Button

import plot_consumption as A

HERE = Path(__file__).parent


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv", help="power-meter CSV to calibrate against")
    ap.add_argument("--dwell", type=float, default=10.0,
                    help="seconds per config (the schedule grid period)")
    ap.add_argument("--n", type=int, default=None,
                    help="number of config windows (default: infer from duration)")
    ap.add_argument("--offset", type=float, default=None,
                    help="initial offset (default: auto-estimate)")
    ap.add_argument("--settle", type=float, default=1.0, help="initial settle (s)")
    ap.add_argument("--window", type=float, default=None,
                    help="initial window length (default: dwell - settle - 1)")
    ap.add_argument("--label", default=None,
                    help="also store offset in offsets.json under this dataset name")
    args = ap.parse_args()

    csv_path = Path(args.csv)
    if not csv_path.is_absolute():
        csv_path = HERE / csv_path
    power = A.load_power(csv_path)
    pt, pw = power["t"], power["w"]
    dwell = args.dwell
    n = args.n if args.n else max(1, int(power["duration"] // dwell))

    # Initial offset from a synthetic uniform schedule (between-window variance).
    synth = {"t_start": np.arange(n) * dwell, "t_end": (np.arange(n) + 1) * dwell}
    auto, lo, hi = A.auto_offset(synth, pt, pw)
    off0 = args.offset if args.offset is not None else auto
    settle0 = max(0.0, min(args.settle, dwell))
    window0 = args.window if args.window is not None else max(0.5, dwell - settle0 - 1.0)
    window0 = max(0.2, min(window0, dwell))

    print(f"loaded {len(pt)} samples, {power['duration']:.0f}s; grid n={n} @ {dwell}s; "
          f"auto offset {auto:+.2f}s")

    fig, ax = plt.subplots(figsize=(14, 6))
    plt.subplots_adjust(bottom=0.30)
    ax.plot(pt, pw, color="0.55", lw=0.8)
    ymin, ymax = np.nanmin(pw), np.nanmax(pw)
    pad = 0.05 * (ymax - ymin + 1e-9)
    ax.set_ylim(ymin - pad, ymax + pad)
    ax.set_xlabel("seconds since power-meter start")
    ax.set_ylabel("power (W)")
    ax.grid(alpha=0.3)
    (mean_line,) = ax.plot([], [], color="red", lw=2.0, zorder=5, label="window mean")
    (grid_line,) = ax.plot([], [], color="0.3", lw=0.5, alpha=0.4)
    ax.legend(loc="upper right")

    state = {"band": None, "off": off0, "settle": settle0, "window": window0}

    def compute():
        off, settle = state["off"], state["settle"]
        window = min(state["window"], max(0.0, dwell - settle))
        rel = pt - off
        k = np.floor(rel / dwell).astype(int)
        phase = rel - k * dwell
        inband = (k >= 0) & (k < n) & (phase >= settle) & (phase < settle + window)
        return off, settle, window, k, inband

    def redraw():
        off, settle, window, k, inband = compute()
        y0, y1 = ax.get_ylim()
        # Measurement windows as transparent rectangle overlays (one collection).
        if state["band"] is not None:
            state["band"].remove()
        xranges = [(off + kk * dwell + settle, window) for kk in range(n)]
        state["band"] = ax.broken_barh(xranges, (y0, y1 - y0),
                                       facecolors="tab:orange", alpha=0.22,
                                       edgecolors="none", zorder=1)
        xs, ys, stds = [], [], []
        for kk in range(n):
            sel = inband & (k == kk)
            if sel.any():
                a = off + kk * dwell + settle
                xs += [a, a + window, np.nan]
                m = float(pw[sel].mean())
                ys += [m, m, np.nan]
                stds.append(float(pw[sel].std()))
        mean_line.set_data(xs, ys)
        gx, gy = [], []
        for kk in range(n + 1):
            x = off + kk * dwell
            gx += [x, x, np.nan]
            gy += [y0, y1, np.nan]
        grid_line.set_data(gx, gy)
        avg_std = float(np.mean(stds)) if stds else float("nan")
        ax.set_title(f"offset={off:+.2f}s  settle={settle:.2f}s  window={window:.2f}s   "
                     f"|  mean band std = {avg_std:.4f} W  (lower = less transient)")
        fig.canvas.draw_idle()

    off_min = min(lo - 2.0, off0 - dwell)
    off_max = max(hi + 2.0, off0 + dwell)
    s_off = Slider(plt.axes([0.12, 0.18, 0.60, 0.03]), "offset (s)", off_min, off_max, valinit=off0)
    s_set = Slider(plt.axes([0.12, 0.13, 0.60, 0.03]), "settle (s)", 0.0, dwell, valinit=settle0)
    s_win = Slider(plt.axes([0.12, 0.08, 0.60, 0.03]), "window (s)", 0.2, dwell, valinit=window0)

    def on_change(_):
        state["off"], state["settle"], state["window"] = s_off.val, s_set.val, s_win.val
        redraw()

    s_off.on_changed(on_change)
    s_set.on_changed(on_change)
    s_win.on_changed(on_change)

    btn = Button(plt.axes([0.80, 0.11, 0.12, 0.05]), "Accept")
    btn.on_clicked(lambda _e: plt.close(fig))

    redraw()
    plt.show()

    off, settle, window, _, _ = compute()
    calib = {"csv": args.csv, "dwell": dwell, "offset": round(off, 3),
             "settle": round(settle, 3), "window": round(window, 3)}
    (HERE / "calibration.json").write_text(json.dumps(calib, indent=2))
    print("\ncalibration:", calib)
    if args.label:
        path = HERE / "offsets.json"
        cache = {}
        if path.exists():
            try:
                cache = json.loads(path.read_text())
            except json.JSONDecodeError:
                pass
        cache[args.label] = {"offset": round(float(off), 3),
                             "settle": round(float(settle), 3),
                             "window": round(float(window), 3),
                             "dwell": dwell}
        path.write_text(json.dumps(cache, indent=2))
        print(f"wrote offsets.json[{args.label}] = {cache[args.label]}")
    print(f"\nUse: python3 plot_consumption.py --only {args.label or '<label>'}")


if __name__ == "__main__":
    main()
