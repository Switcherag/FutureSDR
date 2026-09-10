#!/usr/bin/env python3
"""On-air quick-tune latency, read out of the recorded IQ.

One panel per swap, all on the same axes so they can be compared by eye. Each
shows the transmitted tone's magnitude through the transition: flat on the old
band, the spike that ties the transmit clock to this recording, the instant the
retune was scheduled to fire, the outage, and the moment the tone reappears on
the new band.

The vertical markers come from `quicktune_swap.meta.json` — that is, from what
the binary concluded — while the trace is computed here from the samples. They
are meant to be checked against each other: a marker that does not sit on the
edge it claims is the tell that the number is wrong.

Usage:
    python3 plot_quicktune_swap_iq.py [iq.sc16] [meta.json] [--out plot.png]

Reads the interleaved int16 IQ and sidecar JSON written by
`cargo run --release --bin quicktune_swap_iq`.
"""

import argparse
import json
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
from matplotlib.lines import Line2D

INK = "#0b0b0b"
MUTED = "#898781"
GRID = "#e1e0d9"
SURFACE = "#fcfcfb"
SPINE = "#c3c2b7"

# Three event kinds, three hues, fixed order — never cycled. Validated for CVD
# separation against this surface (worst adjacent pair dE 24.3 protan), and every
# marker is directly labelled as well, so identity never rests on colour alone.
SPIKE = "#1f6fd0"
RETUNE = "#b3541e"
RECOVERY = "#7a3ba8"


def tone_envelope(iq, cyc, win, hop):
    """Magnitude of the tone at `cyc` cycles/sample, per window.

    Mixes the tone down to DC and integrates, which is what separates it from
    the receiver's own noise, from DC offset and from LO leakage — the same
    detector the binary uses, so the trace and the markers are commensurable.
    """
    n = len(iq)
    if n < win:
        return np.zeros(0), np.zeros(0)
    starts = np.arange(0, n - win, hop)
    mixed = iq * np.exp(-2j * np.pi * cyc * np.arange(n))
    csum = np.concatenate(([0], np.cumsum(mixed)))
    vals = np.abs(csum[starts + win] - csum[starts]) / win
    return starts, vals


def dbfs(x):
    return 20.0 * np.log10(np.maximum(x, 1e-9) / 2048.0)


def load(iq_path, meta_path):
    meta = json.loads(Path(meta_path).read_text())
    raw = np.memmap(iq_path, dtype=np.int16, mode="r")
    n = (len(raw) // 2) * 2
    iq = raw[:n].reshape(-1, 2)
    return meta, iq


def slice_complex(iq, lo, hi):
    """Materialise [lo, hi) of the memmap as complex64."""
    lo = max(0, int(lo))
    hi = min(len(iq), int(hi))
    if hi <= lo:
        return np.zeros(0, dtype=np.complex64)
    part = np.asarray(iq[lo:hi], dtype=np.float32)
    return part[:, 0] + 1j * part[:, 1]


def robust_limits(traces, pad_lo=6.0, pad_hi=10.0):
    """Shared y-range that a single near-zero detector window cannot ruin.

    The tone magnitude passes through a deep null mid-retune, and one window
    landing in it reads a couple of hundred dB down. Taking the literal minimum
    lets that one sample set the scale for every panel and squashes the traces
    that carry the answer into the top sliver of the axis. The 1st percentile
    keeps the null visible as a clipped excursion without letting it drive the
    geometry.
    """
    lows = [np.percentile(t, 1) for t in traces if len(t)]
    highs = [t.max() for t in traces if len(t)]
    if not lows:
        return None
    return min(lows) - pad_lo, max(highs) + pad_hi


def style(ax):
    ax.set_facecolor(SURFACE)
    ax.grid(True, color=GRID, lw=0.6, zorder=0)
    ax.set_axisbelow(True)
    for s in ax.spines.values():
        s.set_color(SPINE)
        s.set_linewidth(0.8)
    ax.tick_params(colors=MUTED, labelsize=8, length=3)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("iq", nargs="?", default="quicktune_swap.sc16")
    ap.add_argument("meta", nargs="?", default="quicktune_swap.meta.json")
    ap.add_argument("--out", default="quicktune_swap_iq.png")
    ap.add_argument("--pre-us", type=float, default=400.0,
                    help="microseconds of the old band to show before the retune")
    ap.add_argument("--post-us", type=float, default=900.0,
                    help="microseconds of the new band to show after the retune")
    args = ap.parse_args()

    meta, iq = load(args.iq, args.meta)
    fs = float(meta["sample_rate_hz"])
    ts0 = int(meta["rx_ts0"])
    # Where the carrier lands in the receiver's baseband. Older captures only
    # recorded the baseband tone, which was the same thing back when both LOs
    # were on the same frequency.
    cyc = float(meta.get("carrier_offset_hz", meta.get("tone_hz", 0.0))) / fs
    win = int(meta.get("detector_window", 64))
    events = meta["events"]

    if meta.get("clock_tie", "").startswith("counters"):
        print("NOTE: the clock tie is the coarse counter difference, not a spike. "
              "The markers are only good to a USB round trip.", file=sys.stderr)

    done = [e for e in events if e.get("latency_us") is not None]
    lat = sorted(e["latency_us"] for e in done)

    n_panels = len(events)
    ncol = min(4, max(1, n_panels))
    nrow = (n_panels + ncol - 1) // ncol
    fig = plt.figure(figsize=(3.6 * ncol, 2.5 * nrow + 3.0), facecolor=SURFACE)
    gs = fig.add_gridspec(nrow + 1, ncol, height_ratios=[1.15] + [1] * nrow,
                          hspace=0.55, wspace=0.25)

    # ── Overview: the whole recording, coarsely ──────────────────────────────
    ax0 = fig.add_subplot(gs[0, :])
    style(ax0)
    total = len(iq)
    step = max(1, total // 400_000)
    coarse = slice_complex(iq, 0, total)[::step]
    s, v = tone_envelope(coarse, cyc * step, 256, 64)
    db0 = dbfs(v)
    ax0.plot(s * step / fs * 1e3, db0, color=INK, lw=1.0)
    lim0 = robust_limits([db0], pad_lo=8.0, pad_hi=8.0)
    if lim0:
        ax0.set_ylim(*lim0)
    for e in events:
        if e.get("retune_rx_ts"):
            ax0.axvline((e["retune_rx_ts"] - ts0) / fs * 1e3, color=RETUNE,
                        lw=1.0, alpha=0.7)
    ax0.set_xlabel("time through the recording (ms)", color=MUTED, fontsize=9)
    ax0.set_ylabel("tone (dBFS)", color=MUTED, fontsize=9)
    hop_a, hop_b = events[0]["from_hz"] / 1e6, events[0]["to_hz"] / 1e6
    off = meta.get("tx_offset_hz")
    if off:
        ax0.set_title("", loc="right")
    head = (f"median {lat[len(lat) // 2]:.0f} µs over {len(lat)} swaps"
            if lat else "no swap could be timed")
    ax0.set_title(
        f"On-air quick-tune latency, {hop_a:.0f} ↔ {hop_b:.0f} MHz — {head}",
        color=INK, fontsize=12, loc="left", pad=10)

    # ── One panel per swap, shared scales ────────────────────────────────────
    pre = int(args.pre_us * 1e-6 * fs)
    post = int(args.post_us * 1e-6 * fs)
    axes, traces = [], []
    for k, e in enumerate(events):
        ax = fig.add_subplot(gs[1 + k // ncol, k % ncol])
        style(ax)
        axes.append(ax)
        r = e.get("retune_rx_ts")
        if r is None:
            ax.text(0.5, 0.5, "not timed", transform=ax.transAxes, ha="center",
                    va="center", color=MUTED, fontsize=10)
            ax.set_title(f"swap {k + 1}: {e['from_hz'] / 1e6:.0f} → "
                         f"{e['to_hz'] / 1e6:.0f} MHz", color=MUTED, fontsize=9,
                         loc="left")
            continue
        rp = r - ts0
        seg = slice_complex(iq, rp - pre, rp + post)
        s, v = tone_envelope(seg, cyc, win, max(1, win // 8))
        t = (s + (rp - pre) - rp) / fs * 1e6
        db = dbfs(v)
        traces.append(db)
        ax.plot(t, db, color=INK, lw=1.2, zorder=3)

        # The outage, as a band rather than two more lines.
        rec = e.get("recovery_rx_ts")
        if rec:
            ax.axvspan(0, (rec - r) / fs * 1e6, color=RETUNE, alpha=0.09, zorder=1)
        # Direct labels on the first panel only — the legend carries identity
        # for the rest, and eight copies of the same three words is just ink.
        # "tone back" rides the top because the bottom is where the outage
        # trace is, and a label sitting on the edge it names is unreadable.
        for ts, colour, label, at_top in (
            (e.get("spike_rx_ts"), SPIKE, "spike", False),
            (r, RETUNE, "retune", False),
            (rec, RECOVERY, "tone back", True),
        ):
            if ts is None:
                continue
            x = (ts - r) / fs * 1e6
            ax.axvline(x, color=colour, lw=1.6, zorder=4)
            if k == 0:
                ax.annotate(label,
                            xy=(x, 0.97 if at_top else 0.03),
                            xycoords=("data", "axes fraction"),
                            rotation=90, va="top" if at_top else "bottom",
                            ha="right", fontsize=7, color=colour)
        ax.set_title(
            f"swap {k + 1}: {e['from_hz'] / 1e6:.0f} → {e['to_hz'] / 1e6:.0f} MHz"
            + (f"   {e['latency_us']:.0f} µs" if e.get("latency_us") else ""),
            color=INK, fontsize=9, loc="left")
        if k % ncol == 0:
            ax.set_ylabel("tone (dBFS)", color=MUTED, fontsize=8)
        if k // ncol == nrow - 1:
            ax.set_xlabel("µs from the scheduled retune", color=MUTED, fontsize=8)

    handles = [
        Line2D([], [], color=SPIKE, lw=2, label="spike (clock reference)"),
        Line2D([], [], color=RETUNE, lw=2, label="retune scheduled"),
        Line2D([], [], color=RECOVERY, lw=2, label="tone back on the new band"),
    ]
    leg = fig.legend(handles=handles, loc="upper right", bbox_to_anchor=(0.995, 0.985),
                     ncol=3, frameon=False, fontsize=8.5)
    for t in leg.get_texts():
        t.set_color(MUTED)

    lim = robust_limits(traces)
    if lim:
        for a in axes:
            if a.lines:
                a.set_ylim(*lim)

    fig.savefig(args.out, dpi=160, facecolor=SURFACE, bbox_inches="tight")
    print(f"wrote {args.out}")
    if lat:
        print(f"  n={len(lat)}  min {lat[0]:.1f}  median {lat[len(lat) // 2]:.1f}  "
              f"max {lat[-1]:.1f}  µs")


if __name__ == "__main__":
    main()
