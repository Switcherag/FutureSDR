#!/usr/bin/env python3
"""Discard vs buffered swap policy on one axes: PER against transmit IFS.

Six curves. Colour is the PHY and only the PHY, exactly as in
plot_per_compare.py -- orange 802.15.4, blue 802.11ah, grey the cross-PHY
alternation -- so the same hue means the same radio in every figure. Line style
is the swap policy:

    solid   discard   samples arriving mid-swap are thrown away (the default)
    dashed  buffered  they are kept and delivered late (PLUGIN_HOST_SWAP_BUFFER=1)

No markers: colour carries the PHY and dash the policy, and nothing else needs
encoding.

The IFS axis is corrected for the noise padding inside each cropped reference
frame: the true gap between two PPDUs is the labelled IFS plus one frame's
trailing residual and the next one's leading residual. The residual is derived
from the theoretical PPDU duration, the same way run_benchmark.sh does it:

    802.15.4  PSDU 36 B, O-QPSK 250 kbit/s        -> 1344 us
    802.11ah  MM6108, 2 MHz, MCS0, PSDU 30 B      ->  680 us

The cross-PHY curve alternates the two, so its gaps alternate too; it takes the
mean of the two residuals. Its PER is also halved: the receiver only swaps after
a successful decode, so one failure costs two frames on an alternating stream.

    python3 plot_swap_policy.py                      # -> per_swap_policy.svg
    python3 plot_swap_policy.py --out fig.svg --xmax 1.2 --ymax 60
"""

import argparse
import csv
import math
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.lines import Line2D

HERE = Path(__file__).resolve().parent
EX = HERE.parent                                   # examples/real_device_swap
sys.path.insert(0, str(HERE))
from plot_per_compare import PAPER_RC, PHY_COLOUR  # noqa: E402  same look as the rest

RESULTS = EX / "recording" / "bench" / "results"

# Curve width. Heavier than the paper default because the figure is one column
# wide and carries six overlapping lines with no markers to tell them apart.
LINE_W = 1.8

# (label, results file stem, colour key, unused, frame-residual key)
CURVES = [
    ("802.15.4",  "z2z",   "802.15.4", "o", "z"),
    ("802.11ah",  "h2h",   "802.11ah", "s", "h"),
    ("cross-PHY", "cross", None,       "^", "x"),
]
# (label, results sub-directory, dash). Directories are overridable from the
# command line so an earlier run -- e.g. one recovered from git -- can be
# plotted without overwriting the current results.
POLICIES = [("discard", "fixed", (0, ())), ("buffered", "buffered", (0, (5, 2.5)))]


def residuals_ms():
    """IFS correction per curve, from the frame files and theoretical PPDUs."""
    z_th = (4 + 1 + 1 + 36) * 8 / 4 * 16.0                # us
    h_th = 6 * 40.0 + math.ceil((16 + 30 * 8 + 6) / (52 * 1 * 0.5)) * 40.0
    out = {}
    for key, tag, th in (("z", "zigbee", z_th), ("h", "halow", h_th)):
        n = np.fromfile(EX / "recording" / f"{tag}_frame.cf32", dtype=np.complex64).size
        out[key] = (n / 4e6 * 1e6 - th) / 1000.0
    out["x"] = (out["z"] + out["h"]) / 2
    return out


def load(policy_dir, stem, halve):
    """(ifs_ms, per_pct) points, PER pooled from the raw counts."""
    pts = []
    with open(RESULTS / policy_dir / f"per_replay_{stem}.csv", newline="") as fh:
        for r in csv.DictReader(fh):
            sent = int(r["sent_H"]) + int(r["sent_Z"])
            if sent <= 0:
                continue
            per = 100.0 * (1 - (int(r["rx_H"]) + int(r["rx_Z"])) / sent)
            pts.append((float(r["ifs_ms"]), per / 2 if halve else per))
    return sorted(pts)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=HERE / "per_swap_policy.svg")
    ap.add_argument("--xmax", type=float, default=1.2, help="IFS axis limit, ms")
    ap.add_argument("--ymax", type=float, default=60.0, help="PER axis limit, %%")
    # One journal column. Point sizes are fixed, so a smaller canvas is what
    # makes the text and the curves read large relative to the plot.
    ap.add_argument("--figsize", nargs=2, type=float, default=(3.5, 2.6))
    ap.add_argument("--discard-dir", default="fixed",
                    help="results sub-directory for the discard policy")
    ap.add_argument("--buffered-dir", default="buffered",
                    help="results sub-directory for the buffered policy")
    args = ap.parse_args()
    global POLICIES
    POLICIES = [("discard", args.discard_dir, POLICIES[0][2]),
                ("buffered", args.buffered_dir, POLICIES[1][2])]

    plt.rcParams.update(PAPER_RC)
    fig, ax = plt.subplots(figsize=tuple(args.figsize), layout="constrained")
    shift = residuals_ms()

    for label, stem, ckey, marker, rkey in CURVES:
        colour = PHY_COLOUR.get(ckey, PHY_COLOUR[None])
        for policy, pdir, dash in POLICIES:
            try:
                pts = load(pdir, stem, halve=(stem == "cross"))
            except FileNotFoundError:
                print(f"missing {pdir}/per_replay_{stem}.csv - skipped", file=sys.stderr)
                continue
            x = [p[0] + shift[rkey] for p in pts]
            y = [p[1] for p in pts]
            ax.plot(x, y, color=colour, linestyle=dash, linewidth=LINE_W,
                    solid_capstyle="round", dash_capstyle="round", zorder=3)
            print(f"{label:<10} {policy:<9} shift {shift[rkey]*1000:+5.0f} us  "
                  f"{len(pts)} points")

    ax.set_xlim(0, args.xmax)
    ax.set_ylim(0, args.ymax)
    ax.set_xlabel("Transmit inter-frame spacing (ms)")
    ax.set_ylabel("Packet error rate (%)")
    ax.grid(True, color="#d8d8d8", linewidth=0.4, linestyle=(0, (1, 2)))
    ax.minorticks_on()
    ax.tick_params(which="both", top=True, right=True)

    # One legend: the three PHY colours, a gap, then the two policy line styles.
    # A single box because at one column wide the only region clear of every
    # curve is the top right above ~26% PER, and two stacked legends do not fit
    # there -- the second one landed on the 802.11ah discard curve.
    handles = [Line2D([], [], color=PHY_COLOUR.get(c, PHY_COLOUR[None]),
                      linewidth=LINE_W, label=l)
               for l, _, c, _, _ in CURVES]
    # Black, not grey: grey is already the cross-PHY colour, and a grey "discard"
    # entry was indistinguishable from it. The blank entry splits the two groups.
    handles += [Line2D([], [], color="none", label=" ")]
    handles += [Line2D([], [], color="black", linewidth=LINE_W, linestyle=d, label=p)
                for p, _, d in POLICIES]
    leg = ax.legend(handles=handles, loc="upper right", frameon=True, framealpha=1,
                    edgecolor="#b0b0b0", fancybox=False, handlelength=2.2,
                    borderpad=0.4, labelspacing=0.25)
    leg.get_frame().set_linewidth(0.5)

    fig.savefig(args.out, dpi=600, bbox_inches="tight", pad_inches=0.02, transparent=True)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
