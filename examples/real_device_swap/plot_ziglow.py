#!/usr/bin/env python3
"""Packet error rate against transmit IFS, per PHY, from a `ziglow_swap` capture.

The transmitter sends `--frames-per-step` frames at each IFS step, alternating
HaLow and Zigbee, and sweeps the IFS from `--ifs-start` down to `--ifs-stop`.
Half the frames at each step therefore go to each PHY, and PER is simply

    100 * (1 - received / sent)

per PHY per step. No adjacency, no streak analysis — just how many of the
frames that were sent came back.

The two PHYs are bucketed differently because they carry different stamps:

  * **Zigbee** frames carry the firmware stamp, so `run` *is* the step index
    and `wait_us` *is* that step's IFS. Nothing is inferred.
  * **HaLow** frames are RPG random payload with no stamp — only an 802.11
    sequence number. But the receiver alternates strictly Z, H, Z, H, so every
    HaLow frame sits between two Zigbee frames and inherits the step of the
    one before it. That holds exactly as long as the capture alternates, which
    the script checks and reports.

Several captures can be given and are overlaid: hue carries the PHY, line style
carries the capture, so a SoapySDR run and a quick-tune run of the same sweep
can be read against each other directly.

The sweep is recovered from the data, not from flags — the Zigbee stamp names
its own step and IFS — so a capture with 16 steps of 10 ms and one with 149
steps of 1 ms both plot correctly with no arguments.

Usage:
    python3 plot_ziglow.py [csv ...] [--out plot.png] [--frames-per-step 100]

Defaults to ./ziglow_swap.csv and an interactive window.
"""

import argparse
import csv
import sys
from collections import Counter, defaultdict
from pathlib import Path

import matplotlib.pyplot as plt

# Zigbee red, HaLow blue — one hue per PHY, since PHY is the only thing
# distinguishing the two curves.
COLORS = {"Z": "#d13b30", "H": "#1f6fd0"}
NAMES = {"Z": "Zigbee 2.4 GHz", "H": "HaLow 919 MHz"}
# Line style carries the capture; hue stays with the PHY so the same radio
# reads the same colour in every overlay.
STYLES = ["-", (0, (5, 2)), (0, (1, 1.6))]
CRITICAL = "#d03b3b"
MUTED = "#898781"
GRID = "#e1e0d9"
INK = "#0b0b0b"


def load(path):
    """Return `{phy: {step: received}}`, `{step: ifs_ms}`, and an alternation note."""
    with path.open(newline="") as handle:
        rows = [r for r in csv.DictReader(handle) if r.get("frame_event", "rx") == "rx"]
    if not rows:
        sys.exit(f"{path}: no received frames")

    # The Zigbee stamp is authoritative for both the step index and its IFS.
    ifs_ms = {}
    for r in rows:
        if r["phy"] == "Z" and r["run"] != "-1" and r["wait_us"] != "-1":
            ifs_ms[int(r["run"])] = int(r["wait_us"]) / 1000.0
    if not ifs_ms:
        sys.exit(f"{path}: no stamped Zigbee frames — cannot recover the sweep")

    received = defaultdict(Counter)
    current = None
    orphans = 0
    for r in rows:
        if r["phy"] == "Z" and r["run"] != "-1":
            current = int(r["run"])
            received["Z"][current] += 1
        elif r["phy"] == "H":
            # Inherit the step of the preceding Zigbee frame.
            if current is None:
                orphans += 1
                continue
            received["H"][current] += 1

    seq = "".join(r["phy"] for r in rows)
    breaks = seq.count("ZZ") + seq.count("HH")
    note = f"{len(rows)} frames, {breaks} alternation breaks"
    if orphans:
        note += f", {orphans} HaLow frames before the first stamp (dropped)"
    return received, ifs_ms, note


def summarise(received, ifs_ms, per_phy):
    """`{phy: [(ifs_ms, per_pct, rx, sent), ...]}`, sorted by descending IFS."""
    out = {}
    for phy, counts in received.items():
        series = []
        for step, ifs in sorted(ifs_ms.items()):
            rx = counts.get(step, 0)
            series.append((ifs, 100.0 * (1 - rx / per_phy), rx, per_phy))
        out[phy] = sorted(series, reverse=True)
    return out


def plot(captures, per_phy, title, out):
    """`captures` is `[(label, {phy: [(ifs, per, rx, sent), ...]}), ...]`."""
    fig, ax = plt.subplots(figsize=(11, 6.6), layout="constrained")
    fig.patch.set_facecolor("#fcfcfb")
    ax.set_facecolor("#fcfcfb")

    all_ifs = [d[0] for _, data in captures for s_ in data.values() for d in s_]
    coincident = []
    for slot, (label, data) in enumerate(captures):
        style = STYLES[slot % len(STYLES)]
        # Strict alternation forces both PHYs to the same count at every step:
        # a HaLow frame can only be received after a Zigbee one and vice versa.
        # When that happens the curves are identical and one hides the other,
        # so draw the first as a wide halo and say so rather than shipping a
        # plot that looks like a single PHY was measured.
        vals = [tuple(d[1] for d in series) for series in data.values()]
        same = len(vals) > 1 and len(set(vals)) == 1
        if same:
            coincident.append(label)
        for k, phy in enumerate(sorted(data)):
            series = data[phy]
            halo = same and k == 0
            ax.plot(
                [d[0] for d in series],
                [d[1] for d in series],
                color=COLORS.get(phy, MUTED),
                linewidth=6 if halo else 2,
                alpha=0.4 if halo else 1.0,
                linestyle="-" if halo else style,
                marker=None if halo or len(series) >= 40 else "o",
                markersize=5,
                markeredgecolor="#fcfcfb",
                markeredgewidth=0.6,
                label=f"{NAMES.get(phy, phy)} — {label}" if len(captures) > 1
                      else NAMES.get(phy, phy),
            )

    floor = 100.0 / per_phy  # one frame in per_phy — the measurement resolution
    ax.axhline(floor, color=CRITICAL, linewidth=1, linestyle="--", alpha=0.7)
    ax.annotate(
        f"measurement floor — 1 frame in {per_phy}",
        xy=(max(all_ifs), floor), xytext=(4, 4),
        textcoords="offset points", color=CRITICAL, fontsize=9,
    )

    # A sweep spanning more than a decade crams everything interesting into the
    # left edge on a linear axis, so switch to log when it does.
    if max(all_ifs) / max(min(all_ifs), 1e-9) > 10:
        ax.set_xscale("log")
        ax.set_xticks([2, 5, 10, 20, 50, 100, 150])
        ax.get_xaxis().set_major_formatter(plt.FuncFormatter(lambda v, _: f"{v:g}"))

    ax.set_xlabel("transmit IFS (ms) — sweep runs right to left", color=MUTED)
    ax.set_ylabel("packet error rate (%)", color=MUTED)
    ax.set_title(title, color=INK, loc="left", fontsize=13)
    ax.grid(True, which="both", color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    ax.tick_params(colors=MUTED)
    ax.set_ylim(-2, 102)
    for side, spine in ax.spines.items():
        spine.set_visible(side in ("left", "bottom"))
        spine.set_color("#c3c2b7")
    ax.invert_xaxis()  # sweep order: slowest first
    ax.legend(frameon=False, labelcolor=MUTED, loc="upper left")
    if coincident:
        ax.annotate(
            "the two PHY curves coincide exactly: strict alternation means a frame "
            "on one\nPHY can only follow a frame on the other, so their counts are "
            "locked together",
            xy=(0.5, -0.135), xycoords="axes fraction", ha="center", va="top",
            color=MUTED, fontsize=9,
        )

    if out:
        fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="*", type=Path,
                   help="one or more ziglow captures; several are overlaid")
    p.add_argument("--labels", help="comma-separated legend labels, one per CSV")
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--frames-per-step", type=int, default=100,
                   help="frames sent at each IFS step, both PHYs together (default 100)")
    args = p.parse_args()

    paths = args.csv or [Path("ziglow_swap.csv")]
    for path in paths:
        if not path.exists():
            sys.exit(f"missing {path} — run `ziglow_swap` first")
    labels = args.labels.split(",") if args.labels else [p.stem for p in paths]
    if len(labels) != len(paths):
        sys.exit(f"{len(labels)} labels for {len(paths)} files")

    per_phy = args.frames_per_step // 2  # the sweep alternates, so half each
    captures = []
    for path, label in zip(paths, labels):
        received, ifs_ms, note = load(path)
        data = summarise(received, ifs_ms, per_phy)
        captures.append((label, data))

        vals = [tuple(d[1] for d in series) for series in data.values()]
        print(f"== {path.name}: {note}"
              + ("  [PHY curves identical — locked by alternation]"
                 if len(vals) > 1 and len(set(vals)) == 1 else ""))
        print(f"   {len(ifs_ms)} IFS steps, {per_phy} frames per PHY per step")
        for phy in sorted(data):
            rx = sum(d[2] for d in data[phy])
            sent = sum(d[3] for d in data[phy])
            # First step whose PER clears the floor, i.e. where it breaks down.
            knee = next((d[0] for d in data[phy] if d[1] > 100.0 / per_phy), None)
            print(f"   {NAMES[phy]}: {rx}/{sent} → overall PER "
                  f"{100 * (1 - rx / sent):.1f}%"
                  + (f", first loss at {knee:.0f} ms" if knee else ", no loss at any step"))

    title = (f"{paths[0].stem} — packet error rate vs transmit IFS"
             if len(paths) == 1
             else "packet error rate vs transmit IFS")
    plot(captures, per_phy, title, args.out)


if __name__ == "__main__":
    main()
