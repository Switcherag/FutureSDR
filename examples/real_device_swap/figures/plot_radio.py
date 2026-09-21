#!/usr/bin/env python3
"""PER against the IFS over the air, from radio_bench.sh's runs.

    python3 plot_radio.py RESULTS_DIR [--frames-per-step N] [--ifs-start MS] [--ifs-step MS]

Reads RESULTS_DIR/{zz,ss,gg,gd,11,sz}.csv (those present), a row per
received frame, and writes radio.png and summary.md there.

Which transmission a frame was, and at which IFS:

- ZigBee frames of the multizig firmware carry a stamp: their step, their
  number in the step and the programmed IFS. Exact.
- HaLow frames carry an 802.11 sequence number (12 bits). In the
  alternating run a HaLow frame takes the IFS of the ZigBee frames around
  it. In a HaLow-only run, frames are counted by sequence number from the
  first one received, which is taken as the sweep's first frame, and the
  count is mapped onto the sweep: --frames-per-step frames per spacing,
  from --ifs-start ms down by --ifs-step ms. If the HaLow transmitter does
  otherwise, those curves are wrong; the summary says which were mapped.
"""
import argparse
import csv
from collections import defaultdict
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

RUNS = [
    ("zz", "ZigBee → ZigBee", "#2a78d6", "-"),
    ("ss", "HaLow simple → simple", "#eb6834", "--"),
    ("gg", "HaLow granular → granular", "#1baf7a", "-."),
    ("gd", "HaLow granular, decoder only", "#eda100", ":"),
    ("11", "HaLow single block → single", "#e87ba4", (0, (5, 1, 1, 1))),
    ("sz", "HaLow simple ⇄ ZigBee (retune)", "#008300", (0, (3, 1, 1, 1, 1, 1))),
]
SURFACE, GRID, AXIS = "#fcfcfb", "#e1e0d9", "#c3c2b7"
INK, INK2, MUTED = "#0b0b0b", "#52514e", "#898781"


def rows_of(path):
    return [r for r in csv.DictReader(open(path)) if r["event"] == "rx"]


def zigbee_per(rows):
    """{ifs_ms: (received, expected)} of stamped ZigBee frames."""
    frames = defaultdict(set)  # (step, ifs) -> frame numbers
    for r in rows:
        if r["phy"] == "Z" and int(r["ifs_us"]) >= 0:
            frames[(int(r["step"]), int(r["ifs_us"]))].add(int(r["frame"]))
    per_ifs = defaultdict(lambda: [0, 0])
    for (_, ifs_us), got in frames.items():
        # Numbered from 0 in each step: the highest seen tells how many
        # were sent, at least.
        per_ifs[ifs_us / 1000][0] += len(got)
        per_ifs[ifs_us / 1000][1] += max(got) + 1
    return {k: tuple(v) for k, v in per_ifs.items()}


def halow_between(rows):
    """{ifs_ms: received} of HaLow frames, each at the IFS of the stamped
    ZigBee frame received nearest before it (the alternating run)."""
    got = defaultdict(set)
    last = None
    for r in rows:
        if r["phy"] == "Z" and int(r["ifs_us"]) >= 0:
            last = (int(r["step"]), int(r["ifs_us"]) / 1000)
        elif r["phy"] == "H" and last is not None and int(r["seq"]) >= 0:
            got[last].add(int(r["seq"]))
    per_ifs = defaultdict(int)
    for (_, ifs), seqs in got.items():
        per_ifs[ifs] += len(seqs)
    return per_ifs


def halow_by_schedule(rows, frames_per_step, ifs_start, ifs_step):
    """{ifs_ms: (received, expected)} of HaLow frames counted by sequence
    number from the first received."""
    index, prev, got = 0, None, defaultdict(set)
    for r in rows:
        if r["phy"] != "H" or int(r["seq"]) < 0:
            continue
        seq = int(r["seq"])
        if prev is not None:
            index += (seq - prev) % 4096
        prev = seq
        got[index // frames_per_step].add(index)
    out = {}
    for step, seen in got.items():
        ifs = round(ifs_start - step * ifs_step, 6)
        if ifs < -1e-9:
            continue
        out[ifs] = (len(seen), frames_per_step)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--frames-per-step", type=int, default=1000)
    ap.add_argument("--ifs-start", type=float, default=6.0)
    ap.add_argument("--ifs-step", type=float, default=0.01)
    args = ap.parse_args()
    out = Path(args.dir)

    results, notes = [], []
    for key, label, color, dash in RUNS:
        path = out / f"{key}.csv"
        if not path.exists():
            continue
        rows = rows_of(path)
        z = zigbee_per(rows)
        h_only = not z and any(r["phy"] == "H" for r in rows)
        if h_only:
            per = halow_by_schedule(rows, args.frames_per_step, args.ifs_start, args.ifs_step)
            notes.append(f"{label}: HaLow frames mapped onto the sweep by sequence number")
        elif key == "sz":
            h = halow_between(rows)
            # Alternating: as many HaLow frames sent as ZigBee ones.
            per = {ifs: (rz + h.get(ifs, 0), 2 * ez) for ifs, (rz, ez) in z.items()}
        else:
            per = z
        ifs = sorted(per)
        pers = [100 * (1 - per[i][0] / per[i][1]) if per[i][1] else float("nan") for i in ifs]
        swaps = sorted(float(r["swap_ms"]) for r in rows
                       if r.get("swapped") == "1" and r["swap_ms"] not in ("NaN", "nan"))
        retunes = sorted(float(r["retune_ms"]) for r in rows
                         if r.get("swapped") == "1" and r["retune_ms"] not in ("NaN", "nan"))
        overflows = max((int(r["overflows"]) for r in rows), default=0)
        results.append((key, label, color, dash, ifs, pers, swaps, retunes, overflows,
                        sum(per[i][0] for i in ifs), sum(per[i][1] for i in ifs)))
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
    for key, label, color, dash, ifs, pers, swaps, *_ in results:
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
    png = out / "radio.png"
    fig.savefig(png, dpi=150, facecolor=SURFACE)

    lines = [
        "| Run | Frames received / sent | Swap (median) | Retune (median) | Radio overflows |",
        "|-----|------------------------|---------------|-----------------|-----------------|",
    ]
    for key, label, _, _, _, _, swaps, retunes, overflows, got, sent in results:
        med = lambda v: f"{v[len(v) // 2]:.3f} ms" if v else "–"
        lines.append(f"| {label} | {got} / {sent} | {med(swaps)} | {med(retunes)} | {overflows} |")
    text = f"{system}\n\n" + "\n".join(lines) + "\n"
    if notes:
        text += "\n" + "\n".join(f"- {n}" for n in notes) + "\n"
    (out / "summary.md").write_text(text)
    print(png)
    print(text)


main()
