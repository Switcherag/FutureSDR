#!/usr/bin/env python3
"""Packet error rate over time from a `halow_swap` capture.

`halow_swap` alternates two 802.11ah receive flows on every decoded frame. The
HaLow frames carry no firmware stamp — only the 802.11 sequence number — so PER
is recovered from gaps in that sequence:

    PER = 100 * (1 - received / (last_seq - first_seq + 1))

This is deliberately not `plot_per_sweep.py`. That script buckets by IFS step,
which needs a transmitter sweeping its inter-frame spacing; a constant-IFS
capture like this one has no steps to bucket by. If the capture *does* contain
a sweep, the inter-arrival summary printed below makes it obvious and
`plot_per_sweep.py` is the right tool.

Foreign frames are identified by the sequence number itself rather than by
guessing at payload length. `halow_swap`'s `parse_seq` reads the 802.11
sequence-control field, and a transmitter counts monotonically: a frame that
jumps hundreds ahead and then back belongs to someone else. Three such frames
in a 3000-frame capture were enough to inject +840/-838 steps and destroy the
unwrap, turning a 0.1% PER into nonsense.

The field is 12 bits and wraps every 4096 frames, so a large *negative* step is
a wrap and is unwrapped; anything else implausible is a different transmitter
and its row is dropped. Payload length and fragment number are reported for the
dropped rows as corroboration, never used to decide.

Usage:
    python3 plot_halow_swap.py [csv] [--out plot.png] [--window 200]

Defaults to ./halow_swap.csv and an interactive window.
"""

import argparse
import csv
import statistics
import sys
from collections import Counter
from pathlib import Path

import matplotlib.pyplot as plt

SEQ_MODULO = 4096  # the 802.11 sequence-number field is 12 bits
MAX_PLAUSIBLE_GAP = 512  # a jump larger than this is corruption, not loss
NEIGHBOUR_TOL = 16       # frames either side should be within this many counts

BASE = "#1f6fd0"
FLOWS = {"A": "#1f6fd0", "B": "#d13b30"}
CRITICAL = "#d03b3b"
MUTED = "#898781"
GRID = "#e1e0d9"
INK = "#0b0b0b"


def circ(a, b):
    """Forward distance from `b` to `a` around the 12-bit sequence field."""
    return (a - b) % SEQ_MODULO


def drop_intruders(rows):
    """Remove frames from other transmitters, judged locally.

    A frame is not ours when deleting it makes its two neighbours continuous:
    its own step is a big jump, but the step straight from the previous frame
    to the next one is small. That is a purely local test, which matters —
    judging each frame against a running cursor lets a single intruder poison
    the cursor and reject every legitimate frame after it (measured: 3 real
    intruders became 89 rejections and turned a 0.1% PER into 2.97%).
    """
    seqs = [int(r["seq"]) for r in rows]
    keep, dropped = [], []
    for i, r in enumerate(rows):
        if 0 < i < len(rows) - 1:
            own = circ(seqs[i], seqs[i - 1])
            bridge = circ(seqs[i + 1], seqs[i - 1])
            if own > NEIGHBOUR_TOL and bridge <= NEIGHBOUR_TOL:
                dropped.append(r)
                continue
        keep.append(r)
    return keep, dropped


def load(path):
    """Received rows, in capture order."""
    with path.open(newline="") as handle:
        rows = [r for r in csv.DictReader(handle) if r.get("frame_event", "rx") == "rx"]
    if not rows:
        sys.exit(f"{path}: no received frames")
    return rows


def unwrap(rows):
    """Monotonic frame ids from the 12-bit sequence field.

    Returns `(ids, rows_kept, rejected)`. A negative step of more than half the
    modulus is a wrap; anything else outside a plausible forward gap is a
    corrupted sequence number and its row is rejected rather than unwrapped,
    which would otherwise fabricate hundreds of "lost" frames.
    """
    ids, kept, rejected = [], [], []
    offset = 0
    for r in rows:
        s = int(r["seq"])
        if not ids:
            ids.append(s)
            kept.append(r)
            continue
        prev = ids[-1]
        cand = s + offset
        if cand < prev - SEQ_MODULO // 2:  # wrapped
            offset += SEQ_MODULO
            cand = s + offset
        step = cand - prev
        if step <= 0 or step > MAX_PLAUSIBLE_GAP:
            rejected.append(r)
            continue
        ids.append(cand)
        kept.append(r)
    return ids, kept, rejected


def windowed_per(ids, times, window):
    """`[(t_seconds, per_pct), ...]` over consecutive windows of `window` frames."""
    out = []
    for i in range(0, len(ids) - 1, window):
        chunk = ids[i:i + window + 1]
        if len(chunk) < 2:
            continue
        expected = chunk[-1] - chunk[0] + 1
        out.append((times[i] / 1000.0, 100.0 * (1 - len(chunk) / expected)))
    return out


def plot(series, per_flow, overall, gaps, title, out):
    fig, (ax, ax2) = plt.subplots(
        2, 1, figsize=(11, 7.4), layout="constrained",
        gridspec_kw={"height_ratios": [2, 1]},
    )
    fig.patch.set_facecolor("#fcfcfb")

    ax.set_facecolor("#fcfcfb")
    ax.plot([p[0] for p in series], [p[1] for p in series],
            color=BASE, linewidth=2, marker="o", markersize=4,
            markeredgecolor="#fcfcfb", markeredgewidth=0.5)
    ax.axhline(overall, color=CRITICAL, linewidth=1, linestyle="--", alpha=0.8)
    ax.annotate(f"overall {overall:.2f}%", xy=(series[0][0], overall),
                xytext=(4, 4), textcoords="offset points",
                color=CRITICAL, fontsize=9)
    ax.set_ylabel("packet error rate (%)", color=MUTED)
    ax.set_title(title, color=INK, loc="left", fontsize=13)
    ax.set_ylim(bottom=-0.5)

    # Inter-arrival times say whether the transmitter held one IFS or swept it.
    # A single tight cluster means constant IFS, and PER-vs-IFS is meaningless.
    ax2.set_facecolor("#fcfcfb")
    ax2.hist(gaps, bins=60, color=BASE, alpha=0.85)
    ax2.set_xlabel("inter-arrival time (ms)", color=MUTED)
    ax2.set_ylabel("frames", color=MUTED)
    ax2.set_title("inter-arrival distribution — one cluster means a constant IFS",
                  color=MUTED, loc="left", fontsize=10)

    for a in (ax, ax2):
        a.grid(True, color=GRID, linewidth=0.8)
        a.set_axisbelow(True)
        a.tick_params(colors=MUTED)
        for side, spine in a.spines.items():
            spine.set_visible(side in ("left", "bottom"))
            spine.set_color("#c3c2b7")
    ax.set_xlabel("capture time (s)", color=MUTED)

    if per_flow:
        ax.legend(handles=[
            plt.Line2D([], [], color=FLOWS.get(f, MUTED), linewidth=2,
                       label=f"flow {f}: {n} frames")
            for f, n in sorted(per_flow.items())
        ], frameon=False, labelcolor=MUTED, loc="upper right")

    if out:
        fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="?", type=Path, default=Path("halow_swap.csv"))
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--window", type=int, default=200,
                   help="frames per PER window (default 200)")
    args = p.parse_args()

    if not args.csv.exists():
        sys.exit(f"missing {args.csv} — run `halow_swap` first")

    rows = load(args.csv)
    rows, intruders = drop_intruders(rows)
    ids, kept, rejected = unwrap(rows)
    rejected = intruders + rejected
    if len(ids) < 2:
        sys.exit("not enough frames with a usable sequence number")

    times = [float(r["rx_t_ms"]) for r in kept]
    expected = ids[-1] - ids[0] + 1
    overall = 100.0 * (1 - len(ids) / expected)
    gaps = [b - a for a, b in zip(times, times[1:])]
    per_flow = Counter(r["phy_active"] for r in kept)

    print(f"== {args.csv.name}")
    print(f"   {len(rows) + len(intruders)} rows")
    if rejected:
        print(f"   {len(rejected)} rows dropped — sequence number not from this "
              f"transmitter:")
        for r in rejected[:5]:
            print(f"      seq={r['seq']:>5} frag={r.get('frag','?'):>3} "
                  f"len={r.get('len','?'):>4}  t={float(r['rx_t_ms'])/1000:.1f}s")
    print(f"   received {len(ids)} of {expected} sent → PER {overall:.2f}%")
    print(f"   flows: {dict(per_flow)}")
    print(f"   inter-arrival: median {statistics.median(gaps):.2f} ms, "
          f"min {min(gaps):.2f}, max {max(gaps):.2f}")
    # A sweep shows up as the *typical* gap drifting across the capture. Plain
    # standard deviation cannot see that — a handful of long gaps inflates it
    # and reports a sweep that is not there — so compare the median gap early
    # against late, and quote the interquartile range for spread.
    q = sorted(gaps)
    iqr = q[3 * len(q) // 4] - q[len(q) // 4]
    fifth = max(len(gaps) // 5, 1)
    early = statistics.median(gaps[:fifth])
    late = statistics.median(gaps[-fifth:])
    drift = abs(late - early) / max(early, 1e-9)
    print(f"   IQR {iqr:.2f} ms; median gap {early:.2f} ms early vs "
          f"{late:.2f} ms late ({100 * drift:.1f}% drift)")
    print("   " + ("the IFS is swept — bucket by step with plot_per_sweep.py"
                   if drift > 0.2
                   else "constant IFS, so there is no sweep to plot PER against"))

    series = windowed_per(ids, times, args.window)
    plot(series, per_flow, overall, gaps,
         f"{args.csv.stem} — packet error rate over time", args.out)


if __name__ == "__main__":
    main()
