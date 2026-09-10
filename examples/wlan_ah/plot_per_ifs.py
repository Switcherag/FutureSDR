#!/usr/bin/env python3
"""PER versus inter-frame spacing, from rx_csv logs.

The transmitter sweeps IFS from 10 ms down to 0.1 ms in 0.1 ms steps, sending a
fixed number of frames per step, and pauses between steps. That pause is the
key: step boundaries are *observed* as a large gap in `delta_ms`, not inferred,
so a step's frame count survives even heavy loss within it.

    1. split the log wherever delta_ms exceeds --gap-ms (default 150 ms,
       comfortably under the 300 ms inter-step pause and far above the ~11 ms
       widest in-step gap)
    2. drop runt segments (a partial first step, stray single detections)
    3. the sweep is deterministic, so segment ORDER gives the step: segment 0
       is --ifs-max and each following segment steps down. A receiver that
       loses an entire step emits no segment for it, which would shift every
       later segment, so the drop in slot estimate between neighbours is used
       to detect skipped steps and keep the rest aligned
    4. PER = 1 - received / sent_per_step

Order beats inferring the step from the gaps. Frame loss only ever *inflates*
a gap — lose every second frame and the observed spacing doubles — so any
statistic of delta_ms (median, or even a low percentile) drifts upward exactly
on the lossy steps that matter most, and snaps them to the wrong IFS.

Usage:
    ./plot_per_ifs.py csv/per_*.csv --sent-per-step 100 --out per_vs_ifs.png
"""

import argparse
import os
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

IFS_MAX_MS = 10.0
IFS_MIN_MS = 0.1
IFS_STEP_MS = 0.1


def nominal_steps():
    n = int(round((IFS_MAX_MS - IFS_MIN_MS) / IFS_STEP_MS)) + 1
    return np.round(np.linspace(IFS_MAX_MS, IFS_MIN_MS, n), 3)


def segment(delta, gap_ms):
    """Index arrays, one per transmitted step, split on the inter-step pause."""
    brk = np.where(delta > gap_ms)[0]
    return np.split(np.arange(len(delta)), brk)


def segment_slots(df, gap_ms, min_seg, dedup_ms):
    """(frame count, robust slot estimate) per segment, in transmission order."""
    delta = df["delta_ms"].to_numpy(dtype=float)
    out = []
    for s in segment(delta, gap_ms):
        if len(s) < min_seg:
            continue
        inner = delta[s[1:]]
        # A gap far below one frame's airtime is the same frame decoded twice.
        inner = inner[inner > dedup_ms]
        slot = float(np.percentile(inner, 10)) if inner.size else float("nan")
        out.append((len(s), slot))
    return out


def analyze(df, sent_per_step, gap_ms, min_seg, dedup_ms, steps):
    """Assign segments to steps by order, placing any wholly-lost steps.

    The step count is known, so the number of steps that produced no segment
    at all is exactly `len(steps) - len(segs)`. Rather than deciding skips
    locally — the slot estimate is far too noisy under loss for that — place
    that many, at the largest drops in slot estimate, which is where a missing
    step actually shows up.
    """
    segs = segment_slots(df, gap_ms, min_seg, dedup_ms)
    received = np.zeros(len(steps))
    n_missing = max(len(steps) - len(segs), 0)

    skip_after = set()
    if n_missing and len(segs) > 1:
        slots = np.array([sl for _, sl in segs], dtype=float)
        drops = slots[:-1] - slots[1:]
        drops = np.where(np.isfinite(drops), drops, -np.inf)
        skip_after = set(np.argsort(drops)[-n_missing:].tolist())

    k = 0
    for i, (n_frames, _) in enumerate(segs):
        if k >= len(steps):
            break
        received[k] = n_frames
        k += 1
        if i in skip_after:
            k += 1  # a step nothing was decoded on

    per = 1.0 - received / float(sent_per_step)
    slots = np.array([sl for _, sl in segs], dtype=float)
    ok = np.isfinite(slots)
    trend = (
        float(np.corrcoef(np.arange(len(slots))[ok], slots[ok])[0, 1])
        if ok.sum() > 2
        else float("nan")
    )
    return received, np.clip(per, 0.0, 1.0), len(segs), trend


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("csvs", nargs="+")
    ap.add_argument("--sent-per-step", type=int, default=100)
    ap.add_argument("--gap-ms", type=float, default=150.0,
                    help="delta above this marks an inter-step pause (the "
                         "transmitter pauses 300 ms between steps)")
    ap.add_argument("--min-seg", type=int, default=5,
                    help="ignore segments with fewer frames than this")
    ap.add_argument("--dedup-ms", type=float, default=0.5,
                    help="gaps below this are duplicate decodes, not slots")
    ap.add_argument("--airtime-ms", type=float, default=None)
    ap.add_argument("--out", default="per_vs_ifs.png")
    ap.add_argument("--summary", default=None)
    args = ap.parse_args()

    loaded = []
    for path in args.csvs:
        if not os.path.exists(path):
            print(f"skip (missing): {path}", file=sys.stderr)
            continue
        df = pd.read_csv(path)
        if df.empty:
            print(f"skip (no frames): {path}", file=sys.stderr)
            continue
        # A glob like per_*.csv also matches this script's own --summary
        # output, which has no delta_ms. Say so rather than dying on a KeyError.
        missing = {"delta_ms", "t_rel_s"} - set(df.columns)
        if missing:
            print(f"skip (not an rx_csv log, no {'/'.join(sorted(missing))}): {path}",
                  file=sys.stderr)
            continue
        label = str(df["version"].iloc[0]) if "version" in df else os.path.basename(path)
        loaded.append((label, df))

    if not loaded:
        print("no usable CSVs — nothing plotted", file=sys.stderr)
        return 1

    steps = nominal_steps()

    fig, (ax_per, ax_cnt) = plt.subplots(
        2, 1, figsize=(10, 8), sharex=True, gridspec_kw={"height_ratios": [2, 1]}
    )
    rows = []
    for label, df in loaded:
        received, per, n_seg, trend = analyze(
            df, args.sent_per_step, args.gap_ms, args.min_seg, args.dedup_ms, steps
        )
        covered = len(steps)
        got = float(np.nansum(received))
        overall = 1.0 - got / max(covered * args.sent_per_step, 1)
        lost = len(steps) - n_seg
        warn = "" if lost <= 0 else f"  [{lost} step(s) decoded nothing]"
        print(f"{label:>7}: {len(df):5d} frames, {covered:3d}/{len(steps)} steps, "
              f"overall PER {overall:.3f}, order/slot corr {trend:+.2f}{warn}")
        ax_per.plot(steps, per, marker="o", ms=3, lw=1.4, label=label)
        ax_cnt.plot(steps, received, marker=".", ms=3, lw=1.0, label=label)
        for st, r, p in zip(steps, received, per):
            rows.append({"version": label, "ifs_ms": st,
                         "received": int(r),
                         "sent": args.sent_per_step, "per": p})

    ax_per.set_ylabel("PER")
    ax_per.set_ylim(-0.02, 1.02)
    ax_per.grid(alpha=0.3)
    ax_per.legend(title="receiver")
    ax_per.set_title(
        f"802.11ah PER vs inter-frame spacing "
        f"({IFS_MAX_MS}→{IFS_MIN_MS} ms in {IFS_STEP_MS} ms steps, "
        f"{args.sent_per_step} frames/step)"
    )
    ax_cnt.set_ylabel("frames decoded per step")
    ax_cnt.set_xlabel("inter-frame spacing (ms)")
    ax_cnt.axhline(args.sent_per_step, ls="--", c="k", lw=0.8, alpha=0.5)
    ax_cnt.grid(alpha=0.3)
    ax_cnt.invert_xaxis()

    fig.tight_layout()
    fig.savefig(args.out, dpi=150)
    print(f"wrote {args.out}")
    if args.summary:
        pd.DataFrame(rows).to_csv(args.summary, index=False)
        print(f"wrote {args.summary}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
