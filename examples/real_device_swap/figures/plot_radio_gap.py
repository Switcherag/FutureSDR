#!/usr/bin/env python3
"""PER against the IFS, placing each spacing by the gaps it was received with.

    python3 plot_radio_gap.py RESULTS_DIR [--ifs-start MS] [--ifs-step MS]
                              [--ifs-end MS] [--pause-ms P]

`plot_radio.py` cuts the received frames into spacings at the transmitter's
pauses and takes the n-th part for the n-th spacing. That needs every spacing
to deliver at least one frame: where a whole spacing is lost, two pauses merge,
the part count falls short and every later spacing is placed one step too high.
The run of 2026-09-24 shows it — 301 parts for 593 spacings on the HaLow runs,
while ZigBee, which loses almost nothing, gives 593 of 593.

This places a part by what it measures instead of by its position: inside a
spacing the frames arrive one inter-frame spacing apart, plus the frame and the
transmitter's per-frame overhead, and that overhead is constant across the
sweep. Calibrating it on the first spacing, whose IFS is known (`--ifs-start`),
turns each part's median gap into the IFS it was sent at. A part that is lost
entirely is then simply absent, and the parts around it stay where they belong.

How many frames a spacing holds is read from the data as well: the most common
size among the parts of the first tenth of the sweep, where next to nothing is
lost. The transmitter of the 2026-09-24 run was set to 10 frames a spacing and
delivers 11 on the HaLow runs, so taking it from the firmware would overstate
the loss by 9 %.
"""
import argparse
import csv
from collections import Counter
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

from plot_radio import RUNS, SURFACE, GRID, AXIS, INK, INK2, MUTED


def parts_of(rows, pause_ms):
    """The frames cut into spacings at the pauses: (frames, median gap ms)."""
    times = [float(r["rx_t_ms"]) for r in rows]
    parts, current = [], []
    for t in times:
        if current and t - current[-1] > 0.8 * pause_ms:
            parts.append(current)
            current = []
        current.append(t)
    if current:
        parts.append(current)
    out = []
    for part in parts:
        gaps = sorted(b - a for a, b in zip(part, part[1:]))
        out.append((len(part), gaps[len(gaps) // 2] if gaps else float("nan")))
    return out


def per_ifs(parts, ifs_start, ifs_step, ifs_end):
    """{ifs_ms: (received, sent)}, each part placed by its median gap.

    The gap is IFS + the frames and whatever the transmitter spends between
    them; that offset is taken from the first spacing, which is at
    `ifs_start`. Parts are then snapped to the sweep's grid.
    """
    sized = [p for p in parts if p[0] > 1]
    if not sized:
        return {}, 0
    offset = sized[0][1] - ifs_start
    head = sized[: max(1, len(sized) // 10)]
    sent = Counter(n for n, _ in head).most_common(1)[0][0]
    out = {}
    for n, gap in sized:
        ifs = gap - offset
        if ifs < ifs_end - ifs_step or ifs > ifs_start + ifs_step:
            continue
        ifs = round(round(ifs / ifs_step) * ifs_step, 6)
        got, expected = out.get(ifs, (0, 0))
        out[ifs] = (got + min(n, sent), expected + sent)
    return out, sent


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--ifs-start", type=float, default=6.0)
    ap.add_argument("--ifs-step", type=float, default=0.01)
    ap.add_argument("--ifs-end", type=float, default=0.08)
    ap.add_argument("--pause-ms", type=float, default=500.0)
    args = ap.parse_args()
    out = Path(args.dir)

    results, notes = [], []
    for key, label, color, dash in RUNS:
        path = out / f"{key}.csv"
        if not path.exists():
            continue
        rows = [r for r in csv.DictReader(open(path)) if r["event"] == "rx"]
        if not rows:
            continue
        parts = parts_of(rows, args.pause_ms)
        per, sent = per_ifs(parts, args.ifs_start, args.ifs_step, args.ifs_end)
        if not per:
            continue
        ifs = sorted(per)
        pers = [100 * (1 - per[i][0] / per[i][1]) for i in ifs]
        swaps = sorted(float(r["swap_ms"]) for r in rows
                       if r.get("swapped") == "1" and r["swap_ms"] not in ("NaN", "nan"))
        got = sum(per[i][0] for i in ifs)
        results.append((label, color, dash, ifs, pers, swaps, got, len(parts), sent))
        notes.append(
            f"- {label}: {len(parts)} spacings received of "
            f"{round((args.ifs_start - args.ifs_end) / args.ifs_step) + 1} sent, "
            f"{sent} frames each, placed from {ifs[0]:.2f} to {ifs[-1]:.2f} ms"
        )
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
    ax_zoom.set_xlabel("Inter-frame spacing, measured from the arrivals (ms)",
                       color=INK2, fontsize=11)
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
        "| Run | Frames received / sent | Spacings received | Swap (median) |",
        "|-----|------------------------|-------------------|---------------|",
    ]
    for label, _, _, ifs, _, swaps, got, parts, sent in results:
        med = f"{swaps[len(swaps) // 2]:.3f} ms" if swaps else "–"
        lines.append(f"| {label} | {got} / {parts * sent} | {parts} | {med} |")
    text = f"{system}\n\n" + "\n".join(lines) + "\n\n" + "\n".join(notes) + "\n"
    (out / "summary_gap.md").write_text(text)
    print(png)
    print(text)


main()
