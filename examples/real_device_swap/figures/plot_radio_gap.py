#!/usr/bin/env python3
"""PER against the IFS, one row per spacing, cut at the transmitter's pauses.

    python3 plot_radio_gap.py RESULTS_DIR [--frames-per-step N] [--ifs-start MS]
                              [--ifs-step MS] [--ifs-end MS] [--silence-ms MS]

The transmitter sends a spacing's frames, then falls silent for 500 ms, then
the next spacing. So the rule is simply: **450 ms without a frame starts the
next row**, and row `n` is the n-th spacing of the sweep, at
`ifs_start - n * ifs_step`. It needs no stamp and no sequence number, and it
works the same for ZigBee and for HaLow.

What it does need is that **every spacing delivers at least one frame**. A
spacing that arrives empty merges two pauses into one, the row count falls
short, and every later row is placed one step too high — silently. That is not
hypothetical: on the 2026-09-24 run at 10 frames a spacing, ZigBee gave 593
rows of 593 and the HaLow receivers gave 301, having lost whole spacings. At
100 frames a spacing, losing all of them is far less likely, which is what
makes the rule safe.

So the row count is checked, not assumed, and each row is checked a second way:
inside a spacing the frames arrive one IFS apart plus a constant per-frame
overhead, so the median gap of row `n` should fall by `ifs_step` from row
`n-1`. Taking the offset from the first row, whose IFS is known, gives an
independent estimate of each row's IFS; where the two disagree by more than a
few steps, the rows have drifted and the summary says so.
"""
import argparse
import csv
from collections import Counter
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

from plot_radio import RUNS, SURFACE, GRID, AXIS, INK, INK2, MUTED


def rows_by_silence(frames, silence_ms, pause_ms):
    """One row per spacing: (index, count, median gap ms), and how many
    spacings were inferred empty.

    A row is what arrives between two silences. A spacing that arrives empty
    leaves a longer silence instead of a row — its own frames' worth of time
    plus a second pause — so the silence says how many spacings it swallowed:
    `(silence - pause) / (pause + the time a spacing takes)`, the latter taken
    from the row before it. Without that, one empty spacing would shift every
    row after it one step up the sweep.
    """
    rows, current = [], []
    for t in frames:
        if current and t - current[-1] > silence_ms:
            rows.append(current)
            current = []
        current.append(t)
    if current:
        rows.append(current)

    out, index, skipped = [], 0, 0
    for i, row in enumerate(rows):
        gaps = sorted(b - a for a, b in zip(row, row[1:]))
        gap = gaps[len(gaps) // 2] if gaps else float("nan")
        if i:
            silence = row[0] - rows[i - 1][-1]
            span = rows[i - 1][-1] - rows[i - 1][0]
            period = pause_ms + span
            empty = max(0, round((silence - pause_ms) / period)) if period > 0 else 0
            index += 1 + empty
            skipped += empty
        out.append((index, len(row), gap))
    return out, skipped


def drift(rows, ifs_start, ifs_step, above_ms=1.0):
    """How far the rows' own spacing disagrees with the spacing of their index.

    The gap of a row is its IFS plus the frame and whatever the transmitter
    spends between frames, and that offset is the same all through the sweep;
    the first row, at `ifs_start`, gives it. Only rows above `above_ms` are
    compared: at the bottom the transmitter cannot reach the spacing it asks
    for (~150 us on ZigBee), the gap stops following the programmed IFS, and a
    disagreement there says nothing about the rows being misplaced.

    Returns the median disagreement in steps and the worst one.
    """
    offs = []
    measured = [(i, gap) for i, n, gap in rows if n > 1]
    if len(measured) < 2:
        return 0.0, 0.0
    offset = measured[0][1] - ifs_start
    for i, gap in measured:
        expected = ifs_start - i * ifs_step
        if expected < above_ms:
            continue
        offs.append((gap - offset - expected) / ifs_step)
    if not offs:
        return 0.0, 0.0
    offs.sort()
    return offs[len(offs) // 2], max(offs, key=abs)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--frames-per-step", type=int, default=0,
                    help="frames a spacing holds (default: read from the data)")
    ap.add_argument("--ifs-start", type=float, default=6.0)
    ap.add_argument("--ifs-step", type=float, default=0.01)
    ap.add_argument("--ifs-end", type=float, default=0.08)
    ap.add_argument("--silence-ms", type=float, default=450.0,
                    help="silence that starts the next spacing's row")
    ap.add_argument("--pause-ms", type=float, default=500.0,
                    help="the transmitter's pause between spacings")
    ap.add_argument("--smooth", type=int, default=1,
                    help="average the PER over this many spacings (1: raw)")
    args = ap.parse_args()
    out = Path(args.dir)
    spacings = round((args.ifs_start - args.ifs_end) / args.ifs_step) + 1

    results, notes = [], []
    for key, label, color, dash in RUNS:
        path = out / f"{key}.csv"
        if not path.exists():
            continue
        frames = [float(r["rx_t_ms"]) for r in csv.DictReader(open(path))
                  if r["event"] == "rx"]
        if not frames:
            continue
        rows, skipped = rows_by_silence(frames, args.silence_ms, args.pause_ms)
        # How many frames a spacing holds: the firmware's number, or the most
        # common row size over the first tenth of the sweep, where next to
        # nothing is lost. They differ: the 2026-09-24 transmitter was set to
        # 10 and delivered 11.
        sent = args.frames_per_step or Counter(
            n for _, n, _ in rows[: max(1, len(rows) // 10)]
        ).most_common(1)[0][0]
        # One point per spacing of the sweep, so that spacings that arrived
        # empty are a break in the line rather than a line drawn across them.
        got = [None] * spacings
        for index, n, _ in rows:
            if index < spacings:
                got[index] = min(n, sent)
        ifs = [round(args.ifs_start - i * args.ifs_step, 6) for i in range(spacings)]
        pers = [100 * (1 - g / sent) if g is not None else float("nan") for g in got]
        if args.smooth > 1:
            # A spacing holds few frames, so its PER is coarse: 11 frames can
            # only say 0, 9, 18 %. Averaging over neighbouring spacings shows
            # the trend the single points cannot. Empty spacings are left out
            # of the average rather than counted as total loss.
            w = args.smooth
            smoothed = []
            for i in range(spacings):
                window = [g for g in got[max(0, i - w // 2): i + w // 2 + 1] if g is not None]
                smoothed.append(100 * (1 - sum(window) / (len(window) * sent))
                                if window else float("nan"))
            pers = smoothed
        swaps = sorted(float(r["swap_ms"]) for r in csv.DictReader(open(path))
                       if r["event"] == "rx" and r.get("swapped") == "1"
                       and r["swap_ms"] not in ("NaN", "nan"))
        results.append((label, color, dash, ifs, pers, swaps,
                        sum(n for _, n, _ in rows), len(rows), sent, skipped))
        note = (f"- {label}: {len(rows)} rows for {spacings} spacings, "
                f"{sent} frames each")
        if skipped:
            note += f", {skipped} spacings inferred empty from the long silences"
        placed = rows[-1][0] + 1 if rows else 0
        if placed != spacings:
            note += (f" (WARNING: the rows span {placed} spacings, not {spacings}: "
                     f"the sweep and the settings disagree)")
        median_off, worst = drift(rows, args.ifs_start, args.ifs_step)
        if abs(median_off) > 3:
            note += (f" (WARNING: above 1 ms the rows sit {median_off:+.0f} steps "
                     f"from their own spacing, worst {worst:+.0f})")
        notes.append(note)

    if not results:
        raise SystemExit(f"no runs in {out}")

    fig, (ax_all, ax_zoom) = plt.subplots(2, 1, figsize=(13, 9))
    fig.patch.set_facecolor(SURFACE)
    for ax in (ax_all, ax_zoom):
        ax.set_facecolor(SURFACE)
        ax.grid(True, color=GRID, linewidth=0.8)
        ax.set_axisbelow(True)
        for side in ("top", "right"):
            ax.spines[side].set_visible(False)
        for side in ("left", "bottom"):
            ax.spines[side].set_color(AXIS)
        ax.tick_params(colors=INK2, labelsize=10)
    for label, color, dash, ifs, pers, swaps, *_ in results:
        swap = swaps[len(swaps) // 2] if swaps else float("nan")
        style = dict(color=color, linewidth=1.6, linestyle=dash,
                     label=f"{label} (swap {swap:.3f} ms)")
        ax_all.plot(ifs, pers, **style)
        zoom = [(i, p) for i, p in zip(ifs, pers) if i <= 1.0]
        ax_zoom.plot([i for i, _ in zoom], [p for _, p in zoom], **style)
    ax_all.set_title("Packet error rate, the whole sweep", loc="left", color=INK2, fontsize=11)
    ax_zoom.set_title("Packet error rate, 0 to 1 ms", loc="left", color=INK2, fontsize=11)
    for ax in (ax_all, ax_zoom):
        ax.set_ylabel("PER (%)", color=INK2, fontsize=11)
        ax.set_ylim(-2, 102)
    ax_zoom.set_xlabel("Programmed inter-frame spacing (ms)", color=INK2, fontsize=11)
    ax_all.legend(loc="upper left", bbox_to_anchor=(1.01, 1.0), frameon=False,
                  fontsize=9.5, labelcolor=INK2)
    system = (out / "system.txt").read_text().splitlines()[0] if (out / "system.txt").exists() else ""
    fig.suptitle("Receiver replaced after every frame, over the air (bladeRF)",
                 x=0.06, y=0.99, ha="left", color=INK, fontsize=14, fontweight="bold")
    fig.text(0.06, 0.955, system, ha="left", va="top", color=MUTED, fontsize=9.5)
    fig.tight_layout(rect=(0, 0, 0.72, 0.94))
    png = out / "radio_gap.png"
    fig.savefig(png, dpi=150, facecolor=SURFACE)

    lines = [
        "| Run | Frames received / sent | Spacings | Swap (median) |",
        "|-----|------------------------|----------|---------------|",
    ]
    for label, _, _, _, _, swaps, got, nrows, sent, _ in results:
        med = f"{swaps[len(swaps) // 2]:.3f} ms" if swaps else "–"
        lines.append(f"| {label} | {got} / {nrows * sent} | {nrows} of {spacings} | {med} |")
    text = f"{system}\n\n" + "\n".join(lines) + "\n\n" + "\n".join(notes) + "\n"
    (out / "summary_gap.md").write_text(text)
    print(png)
    print(text)


main()
