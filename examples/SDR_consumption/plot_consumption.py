#!/usr/bin/env python3
"""Plot bladeRF power consumption vs each swept parameter.

Pairs the blade power-meter CSVs with the schedule logs emitted by the Rust
binaries (sdr_consumption_tx.csv / sdr_consumption_rx.csv), aligns them with the
per-dataset calibration in offsets.json (offset / settle / window from
calibrate.py), reduces each config to a mean power, and draws one clean figure:

    bandwidth vs mean | waveform vs mean | frequency vs mean | gain vs mean

    python3 plot_consumption.py            # show
    python3 plot_consumption.py --no-show  # save PNG only
"""
import argparse
import csv
import json
import pickle
import re
from datetime import datetime
from pathlib import Path

import numpy as np
import matplotlib.pyplot as plt

HERE = Path(__file__).parent
OFFSETS_CACHE = HERE / "offsets.json"
SIGNAL_ORDER = ["constant", "sine", "noise"]

# blade only: power CSV  +  the schedule log from that run
DATASETS = {
    "blade_tx": {"power": "csv/blade_tx.csv", "log": "sdr_consumption_tx.csv"},
    "blade_rx": {"power": "csv/blade_rx.csv", "log": "sdr_consumption_rx.csv"},
}

_NUM_RE = re.compile(r"[-+]?\d*\.?\d+(?:[eE][-+]?\d+)?")
_DT_FORMATS = ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S.%f",
               "%Y/%m/%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S")


# ─── parsing ──────────────────────────────────────────────────────────────
def _num(cell):
    m = _NUM_RE.search(cell or "")
    return float(m.group()) if m else np.nan


def _parse_dt(s):
    s = s.strip()
    for fmt in _DT_FORMATS:
        try:
            return datetime.strptime(s, fmt)
        except ValueError:
            pass
    return datetime.fromisoformat(s)


def _spread_subsecond(t):
    t = np.asarray(t, dtype=float)
    out = t.copy()
    n, i = len(t), 0
    while i < n:
        j = i
        while j < n and t[j] == t[i]:
            j += 1
        nxt = t[j] if j < n else t[i] + 1.0
        span = nxt - t[i] if nxt > t[i] else 1.0
        for m in range(j - i):
            out[i + m] = t[i] + span * m / (j - i)
        i = j
    return out


def load_power(path):
    """Load a power-meter CSV → dict of seconds-since-start + channels."""
    with open(path, newline="") as f:
        head = f.readline()
    delim = "\t" if head.count("\t") > head.count(",") else ","
    with open(path, newline="") as f:
        reader = csv.reader(f, delimiter=delim)
        header = [h.strip().lower() for h in next(reader)]
        rows = [r for r in reader if r]

    def col(*keys):
        for i, h in enumerate(header):
            if any(k in h for k in keys):
                return i
        return None

    ci = {"time": col("time", "date"), "unix": col("unix"),
          "v": col("voltage", "volt"), "a": col("current", "amp"),
          "w": col("power", "watt"), "t": col("temperature", "temp")}
    if ci["w"] is None or (ci["time"] is None and ci["unix"] is None):
        raise ValueError(f"{path}: missing Power / Time columns in {header}")

    if ci["unix"] is not None:
        keys = np.array([_num(r[ci["unix"]]) if ci["unix"] < len(r) else np.nan for r in rows])
        good = np.isfinite(keys)
        rows = [r for r, g in zip(rows, good) if g]
        keys = keys[good]
        order = np.argsort(keys, kind="stable")
        rows = [rows[i] for i in order]
        secs = keys[order] - keys[order][0]
    else:
        parsed = []
        for r in rows:
            try:
                parsed.append((_parse_dt(r[ci["time"]]), r))
            except (ValueError, IndexError):
                pass
        parsed.sort(key=lambda x: x[0])
        rows = [r for _, r in parsed]
        secs = _spread_subsecond([(dt - parsed[0][0]).total_seconds() for dt, _ in parsed])

    if not rows:
        raise ValueError(f"{path}: no data rows parsed")

    def grab(idx):
        if idx is None:
            return np.full(len(rows), np.nan)
        return np.array([_num(r[idx]) if idx < len(r) else np.nan for r in rows])

    return {"t": np.asarray(secs, float), "v": grab(ci["v"]), "a": grab(ci["a"]),
            "w": grab(ci["w"]), "temp": grab(ci["t"]),
            "duration": float(secs[-1] - secs[0])}


def load_log(path):
    """Load the program schedule CSV → dict of arrays + kind (rx/tx)."""
    with open(path, newline="") as f:
        reader = csv.DictReader(f)
        fields = [c.strip() for c in (reader.fieldnames or [])]
        recs = list(reader)
    if not recs:
        raise ValueError(f"{path}: empty program log")
    kind = "tx" if "signal" in fields else "rx"
    return {
        "t_start": np.array([float(r["t_start_s"]) for r in recs]),
        "t_end": np.array([float(r["t_end_s"]) for r in recs]),
        "freq": np.array([float(r["freq_hz"]) for r in recs]),
        "bw": np.array([float(r["bw_hz"]) for r in recs]),
        "gain": np.array([float(r["gain_db"]) for r in recs]),
        "signal": [r.get("signal", "") for r in recs] if kind == "tx" else [""] * len(recs),
        "kind": kind,
    }


# ─── alignment / aggregation (auto_offset + load_power used by calibrate.py) ─
def within_ss(log, pt, pw, off, settle=0.0):
    total_ss, total_n = 0.0, 0
    for a0, b0 in zip(log["t_start"], log["t_end"]):
        m = (pt >= a0 + off + settle) & (pt < b0 + off)
        if int(m.sum()) >= 2:
            v = pw[m]
            total_ss += float(np.sum((v - v.mean()) ** 2))
            total_n += int(m.sum())
    return total_ss / max(total_n, 1)


def between_ss(log, pt, pw, off, min_samples=3):
    means, ns = [], []
    for a0, b0 in zip(log["t_start"], log["t_end"]):
        m = (pt >= a0 + off) & (pt < b0 + off)
        k = int(m.sum())
        if k < min_samples:
            return -1.0, False
        means.append(float(pw[m].mean()))
        ns.append(k)
    means, ns = np.array(means), np.array(ns)
    return float(np.sum(ns * (means - np.average(means, weights=ns)) ** 2)), True


def auto_offset(log, pt, pw):
    prog_dur = float(log["t_end"][-1] - log["t_start"][0])
    pdur = float(pt[-1] - pt[0])
    lo, hi = (0.0, pdur - prog_dur) if pdur > prog_dur else (-2.0, max(0.5, pdur - prog_dur) + 2.0)
    grid = np.arange(lo, hi + 1e-9, 0.1)
    if len(grid) == 0:
        return 0.0, -5.0, 5.0
    best = None
    for off in grid:
        b, ok = between_ss(log, pt, pw, off)
        if ok and (best is None or b > best[1]):
            best = (float(off), b)
    if best is not None:
        return best[0], lo, hi
    return float(grid[int(np.argmin([within_ss(log, pt, pw, o) for o in grid]))]), lo, hi


def window_stats(power, log, off, settle, window):
    """Per-config mean power/current over the calibrated measurement window."""
    pt = power["t"]
    out = []
    for i in range(len(log["t_start"])):
        a = log["t_start"][i] + off + settle
        b = log["t_end"][i] + off if window is None else min(log["t_end"][i] + off, a + window)
        m = (pt >= a) & (pt < b)
        n = int(m.sum())
        out.append({
            "freq_hz": log["freq"][i], "bw_hz": log["bw"][i], "gain_db": log["gain"][i],
            "signal": log["signal"][i], "n_samples": n,
            "power_mean_w": float(np.mean(power["w"][m])) if n else np.nan,
            "current_mean_a": float(np.mean(power["a"][m])) if n else np.nan,
        })
    return out


def load_offsets():
    if OFFSETS_CACHE.exists():
        try:
            return json.loads(OFFSETS_CACHE.read_text())
        except json.JSONDecodeError:
            pass
    return {}


def _avg_by(stats, key):
    vals = {}
    for r in stats:
        if np.isfinite(r["power_mean_w"]):
            vals.setdefault(r[key], []).append(r["power_mean_w"])
    xs = sorted(vals)
    return xs, [float(np.mean(vals[x])) for x in xs]


# ─── driver ───────────────────────────────────────────────────────────────
def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--no-show", action="store_true", help="save PNG only, don't display")
    args = ap.parse_args()

    cache = load_offsets()
    datasets = {}
    for label, spec in DATASETS.items():
        ppath, lpath = HERE / spec["power"], HERE / spec["log"]
        if not ppath.exists() or not lpath.exists():
            miss = [p.name for p in (ppath, lpath) if not p.exists()]
            print(f"[{label}] missing {miss}, skipping")
            continue
        power, log = load_power(ppath), load_log(lpath)
        cal = cache.get(label, {})
        if isinstance(cal, dict):
            off, settle, window = cal.get("offset"), cal.get("settle", 1.0), cal.get("window")
        else:
            off, settle, window = cal, 1.0, None
        if off is None:
            off = auto_offset(log, power["t"], power["w"])[0]
        stats = window_stats(power, log, off, settle, window)
        gp = float(np.nanmean([r["power_mean_w"] for r in stats]))
        print(f"[{label}] {log['kind']}, {len(stats)} cfg, offset {off:+.2f}s -> {gp:.3f} W")
        datasets[label] = stats

    if not datasets:
        print("No blade datasets found.")
        return

    fig, ax = plt.subplots(2, 2, figsize=(12, 9))

    for label, st in datasets.items():
        x, y = _avg_by(st, "bw_hz")
        ax[0, 0].plot([v / 1e6 for v in x], y, "o-", label=label)
        x, y = _avg_by(st, "freq_hz")
        ax[1, 0].plot([v / 1e6 for v in x], y, "o-", label=label)
        x, y = _avg_by(st, "gain_db")
        ax[1, 1].plot(x, y, "o-", label=label)
        sigs = [s for s in SIGNAL_ORDER if any(r["signal"] == s for r in st)]
        if sigs:
            ys = [float(np.nanmean([r["power_mean_w"] for r in st if r["signal"] == s]))
                  for s in sigs]
            ax[0, 1].plot(range(len(sigs)), ys, "o-", label=label)
            ax[0, 1].set_xticks(range(len(sigs)))
            ax[0, 1].set_xticklabels(sigs)

    ax[0, 0].set(title="mean power vs bandwidth", xlabel="bandwidth (MHz)", ylabel="mean power (W)")
    ax[0, 1].set(title="mean power vs waveform", xlabel="waveform", ylabel="mean power (W)")
    ax[1, 0].set(title="mean power vs frequency", xlabel="frequency (MHz)", ylabel="mean power (W)")
    ax[1, 1].set(title="mean power vs gain", xlabel="gain (dB)", ylabel="mean power (W)")
    for a in ax.flat:
        a.grid(alpha=0.3)
        if a.get_legend_handles_labels()[0]:
            a.legend()
    fig.suptitle("bladeRF power consumption", fontsize=14)
    fig.tight_layout()
    fig.savefig(HERE / "consumption_blade.png", dpi=120)
    with open(HERE / "consumption_blade.fig.pickle", "wb") as f:
        pickle.dump(fig, f)
    print("wrote consumption_blade.png + consumption_blade.fig.pickle")
    if not args.no_show:
        plt.show()


if __name__ == "__main__":
    main()
