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
    """One row per spacing: (index, count, median gap ms, median swap ms), and how many
    spacings were inferred empty.

    A row is what arrives between two silences. A spacing that arrives empty
    leaves a longer silence instead of a row — its own frames' worth of time
    plus a second pause — so the silence says how many spacings it swallowed:
    `(silence - pause) / (pause + the time a spacing takes)`, the latter taken
    from the row before it. Without that, one empty spacing would shift every
    row after it one step up the sweep.
    """
    rows, current = [], []
    for frame in frames:
        if current and frame[0] - current[-1][0] > silence_ms:
            rows.append(current)
            current = []
        current.append(frame)
    if current:
        rows.append(current)

    def middle(values):
        values = sorted(v for v in values if v == v)
        return values[len(values) // 2] if values else float("nan")

    out, index, skipped = [], 0, 0
    for i, row in enumerate(rows):
        times = [t for t, _ in row]
        gap = middle([b - a for a, b in zip(times, times[1:])])
        swap = middle([s for _, s in row])
        if i:
            silence = row[0][0] - rows[i - 1][-1][0]
            span = rows[i - 1][-1][0] - rows[i - 1][0][0]
            period = pause_ms + span
            empty = max(0, round((silence - pause_ms) / period)) if period > 0 else 0
            index += 1 + empty
            skipped += empty
        out.append((index, len(row), gap, swap))
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
    measured = [(i, gap) for i, n, gap, _ in rows if n > 1]
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


PHY_KEYS = ["zz", "zc", "sv", "sz"]
HALOW_KEYS = ["sv", "si", "gv", "gi", "1v", "1i"]


def figure(runs, keys, title, subtitle, ylabel, ymax, value, path):
    """One panel, PER or swap time against the IFS, for `keys` of `runs`."""
    fig, ax = plt.subplots(figsize=(11, 5.5))
    fig.patch.set_facecolor(SURFACE)
    ax.set_facecolor(SURFACE)
    ax.grid(True, color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(AXIS)
    ax.tick_params(colors=INK2, labelsize=10)
    for key in keys:
        run = runs.get(key)
        if not run:
            continue
        # A spacing holds few frames, so its PER can only take a few values —
        # eleven frames can say 0, 9, 18 % and nothing between — and only the
        # spacings that delivered a frame have a value at all. Drawing that as
        # a line invents both the values between the steps and the spacings
        # between the points, so each spacing is a point.
        # Colour says which receiver, filled or hollow says what a swap
        # replaced: the whole flowgraph, or that one block in place.
        in_place = run["dash"] == (0, (1, 1))
        ax.plot(run["ifs"], value(run), color=run["color"], marker="o",
                markersize=3.2, linestyle="none", alpha=0.7,
                markerfacecolor="none" if in_place else run["color"],
                markeredgewidth=0.9, label=run["label"])
        if trend := run.get("trend"):
            ax.plot(run["ifs"], trend(run), color=run["color"], linewidth=1.4,
                    linestyle=run["dash"], alpha=0.9)
    # The sweep spends most of its steps below 1 ms, where everything happens;
    # a log axis gives that end the room the linear one spent on the top.
    ax.set_xscale("log")
    ax.set_xticks([0.08, 0.1, 0.2, 0.5, 1, 2, 4, 6])
    ax.get_xaxis().set_major_formatter(matplotlib.ticker.ScalarFormatter())
    ax.set_xlim(0.075, 6.5)
    ax.set_ylim(-ymax * 0.02, ymax)
    ax.set_xlabel("Programmed inter-frame spacing (ms)", color=INK2, fontsize=11)
    ax.set_ylabel(ylabel, color=INK2, fontsize=11)
    legend = ax.legend(loc="upper left", bbox_to_anchor=(1.01, 1.0), frameon=False,
                       fontsize=9.5, labelcolor=INK2, markerscale=3)
    for handle in legend.legend_handles:
        handle.set_alpha(1.0)
    fig.suptitle(title, x=0.045, y=0.99, ha="left", color=INK, fontsize=14,
                 fontweight="bold")
    fig.text(0.045, 0.925, subtitle, ha="left", va="top", color=MUTED, fontsize=9.5)
    fig.tight_layout(rect=(0, 0, 0.78, 0.9))
    fig.savefig(path, dpi=150, facecolor=SURFACE)
    plt.close(fig)


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
                    help="average over this many spacings (1: raw)")
    args = ap.parse_args()
    out = Path(args.dir)
    spacings = round((args.ifs_start - args.ifs_end) / args.ifs_step) + 1

    def over(values, sent=None):
        """`values` per spacing, averaged over `--smooth` neighbours, with the
        spacings that arrived empty left out rather than counted as loss."""
        w = max(1, args.smooth)
        out = []
        for i in range(spacings):
            window = [v for v in values[max(0, i - w // 2): i + w // 2 + 1] if v is not None]
            out.append(sum(window) / len(window) if window else float("nan"))
        return out

    runs, notes = {}, []
    for key, label, color, dash in RUNS:
        path = out / f"{key}.csv"
        if not path.exists():
            continue
        frames = [(float(r["rx_t_ms"]),
                   float(r["swap_ms"]) if r["swap_ms"] not in ("NaN", "nan") else float("nan"))
                  for r in csv.DictReader(open(path)) if r["event"] == "rx"]
        if not frames:
            continue
        rows, skipped = rows_by_silence(frames, args.silence_ms, args.pause_ms)
        # How many frames a spacing holds: the firmware's number, or the most
        # common row size over the first tenth of the sweep, where next to
        # nothing is lost. They differ: the 2026-09-24 transmitter was set to
        # 10 and delivered 11.
        sent = args.frames_per_step or Counter(
            n for _, n, _, _ in rows[: max(1, len(rows) // 10)]
        ).most_common(1)[0][0]

        got = [None] * spacings
        swap = [None] * spacings
        for index, n, _, s in rows:
            if index < spacings:
                got[index] = min(n, sent)
                swap[index] = s if s == s else None
        runs[key] = {
            "label": label, "color": color, "dash": dash,
            "ifs": [round(args.ifs_start - i * args.ifs_step, 6) for i in range(spacings)],
            "per": over([100 * (1 - g / sent) if g is not None else None for g in got]),
            "swap": over(swap),
            "frames": sum(n for _, n, _, _ in rows), "rows": len(rows), "sent": sent,
        }
        note = (f"- {label}: {len(rows)} rows for {spacings} spacings, "
                f"{sent} frames each, {runs[key]['frames']} frames")
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

    if not runs:
        raise SystemExit(f"no runs in {out}")
    system = (out / "system.txt").read_text().splitlines()[0] if (out / "system.txt").exists() else ""

    figure(runs, PHY_KEYS, "Packet error rate: one receiver of each kind", system,
           "PER (%)", 100, lambda r: r["per"], out / "per_phy.png")
    figure(runs, HALOW_KEYS, "Packet error rate: the HaLow receivers", system,
           "PER (%)", 100, lambda r: r["per"], out / "per_halow.png")
    figure(runs, [k for k, *_ in RUNS if k in runs],
           "Time to replace the receiver, against the spacing it works at", system,
           "Swap (median per spacing, ms)", 1.2, lambda r: r["swap"], out / "swap.png")

    lines = [
        "| Run | Frames received / sent | Spacings | Swap (median) |",
        "|-----|------------------------|----------|---------------|",
    ]
    for key, *_ in RUNS:
        r = runs.get(key)
        if not r:
            continue
        swaps = sorted(s for s in r["swap"] if s == s)
        med = f"{swaps[len(swaps) // 2]:.3f} ms" if swaps else "–"
        lines.append(f"| {r['label']} | {r['frames']} / {r['rows'] * r['sent']} | "
                     f"{r['rows']} of {spacings} | {med} |")
    text = f"{system}\n\n" + "\n".join(lines) + "\n\n" + "\n".join(notes) + "\n"
    (out / "summary_gap.md").write_text(text)
    print("\n".join(str(out / n) for n in ("per_phy.png", "per_halow.png", "swap.png")))
    print(text)


if __name__ == "__main__":
    main()
