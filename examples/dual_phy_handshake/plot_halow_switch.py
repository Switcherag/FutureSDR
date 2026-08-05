#!/usr/bin/env python3
"""Plot receive time against 802.11 sequence number from a `halow_switch` CSV.

The CSV's `rftap_hex` column holds the RFTAP-wrapped frame (DLT 105 =
IEEE802_11). This unwraps it, pulls the BSSID and the sequence number out of
the MAC header, and plots one point per received frame:

    x = receive timestamp        y = sequence number       color = BSSID

Because the transmitter increments the sequence number on every frame it sends,
a straight line means nothing was missed and a vertical jump means frames went
by while the receiver was not listening — which is exactly what the A/B flow
swap costs. Segments spanning a gap are drawn in red and the misses are
summarised on stdout.

Usage:
    python3 plot_halow_switch.py [csv] [--out plot.png] [--time relative|epoch|elapsed]

Defaults to ./halow_switch.csv and an interactive window.
"""

import argparse
import csv
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.collections import LineCollection
from matplotlib.lines import Line2D

DLT_IEEE802_11 = 105
SEQ_MODULO = 4096  # the sequence number field is 12 bits

# Categorical slots, assigned to BSSIDs in first-seen order (never cycled —
# past the eighth BSSID the script stops rather than inventing a hue).
SERIES = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#4a3aa7", "#e34948"]
CRITICAL = "#d03b3b"  # status: frames missed between two receptions
MUTED = "#898781"     # axis / label ink
GRID = "#e1e0d9"
INK = "#0b0b0b"
# One marker per listen flow, so flow identity is never carried by color alone.
MARKERS = {"A": "o", "B": "s"}


def parse_rftap(blob):
    """Return the encapsulated frame from an RFTAP blob, or None.

    Layout: magic `RFta`, u16 header length in 32-bit words, u16 present-flags,
    then the optional fields — with bit 0 set, the first is the u32 DLT.
    """
    if len(blob) < 12 or blob[0:4] != b"RFta":
        return None
    header_len = int.from_bytes(blob[4:6], "little") * 4
    present = int.from_bytes(blob[6:8], "little")
    if header_len < 12 or len(blob) < header_len or not present & 1:
        return None
    dlt = int.from_bytes(blob[8:12], "little")
    if dlt != DLT_IEEE802_11:
        return None
    return blob[header_len:]


def parse_dot11(frame):
    """Return `(bssid, seq, frag)` from an 802.11 MAC header, or None.

    Which address holds the BSSID depends on the DS bits; a 4-address WDS frame
    has no single BSSID and control frames carry no sequence control at all.
    """
    if len(frame) < 24:
        return None
    ftype = (frame[0] >> 2) & 0x3
    if ftype == 1:  # control frames: no addr3, no sequence control
        return None
    flags = frame[1]
    to_ds, from_ds = flags & 0x1, (flags >> 1) & 0x1
    if to_ds and from_ds:
        return None  # WDS: addr1..4 are RA/TA/DA/SA, no BSSID field
    addr = {(0, 0): frame[16:22], (1, 0): frame[4:10], (0, 1): frame[10:16]}[(to_ds, from_ds)]
    bssid = ":".join(f"{b:02x}" for b in addr)
    seq_ctl = int.from_bytes(frame[22:24], "little")
    return bssid, seq_ctl >> 4, seq_ctl & 0xF


def load(path, time_mode):
    """Read the CSV into per-BSSID lists of `(x, seq, flow, row)`."""
    with path.open(newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        sys.exit(f"{path}: no rows")

    t0 = float(rows[0]["rx_epoch_s"])
    frames, skipped = [], 0
    for row in rows:
        frame = parse_rftap(bytes.fromhex(row["rftap_hex"]))
        parsed = parse_dot11(frame) if frame else None
        if parsed is None:
            skipped += 1
            continue
        bssid, seq, _frag = parsed
        if time_mode == "epoch":
            x = float(row["rx_epoch_s"])
        elif time_mode == "elapsed":
            x = float(row["elapsed_s"])
        else:
            x = float(row["rx_epoch_s"]) - t0
        frames.append((x, seq, row["flow"], bssid))

    if not frames:
        sys.exit(f"{path}: no 802.11 frames with a sequence number ({skipped} rows skipped)")

    by_bssid = defaultdict(list)
    for x, seq, flow, bssid in frames:
        by_bssid[bssid].append((x, seq, flow))
    return by_bssid, skipped


def unwrap(seqs):
    """Undo the 12-bit wrap so the series stays monotonic across 4095 -> 0.

    Only unwraps a decrease large enough to be a wrap rather than reordering.
    """
    out, offset = [], 0
    for i, seq in enumerate(seqs):
        if i and seq + offset < out[-1] - SEQ_MODULO // 2:
            offset += SEQ_MODULO
        out.append(seq + offset)
    return out


def gaps(seqs):
    """Frames missed before each point (0 for the first, and for duplicates)."""
    return [0] + [max(b - a - 1, 0) for a, b in zip(seqs, seqs[1:])]


def summarise(by_bssid, series_seq, skipped):
    if skipped:
        print(f"skipped {skipped} row(s): not an 802.11 frame with a sequence number")
    for bssid, points in by_bssid.items():
        seqs = series_seq[bssid]
        missed = gaps(seqs)
        total = sum(missed)
        received = len(points)
        span = seqs[-1] - seqs[0] + 1
        by_flow = defaultdict(int)
        for _x, _s, flow in points:
            by_flow[flow] += 1
        flows = ", ".join(f"{f}={by_flow[f]}" for f in sorted(by_flow))
        print(
            f"{bssid}: {received} frames ({flows}), seq {seqs[0] % SEQ_MODULO}"
            f"..{seqs[-1] % SEQ_MODULO} spanning {span}"
        )
        print(
            f"    missed {total} ({total / span:.1%} of the span), "
            f"{sum(1 for m in missed if m)} gap(s), largest {max(missed)}"
        )
        deltas = [b - a for a, b in zip([p[0] for p in points], [p[0] for p in points][1:])]
        if deltas:
            print(
                f"    inter-arrival: min {min(deltas) * 1e3:.1f} ms, "
                f"median {sorted(deltas)[len(deltas) // 2] * 1e3:.1f} ms, "
                f"max {max(deltas) * 1e3:.1f} ms"
            )


def plot(by_bssid, series_seq, xlabel, ylabel, title, out):
    fig, ax = plt.subplots(figsize=(11, 6), layout="constrained")
    fig.patch.set_facecolor("#fcfcfb")
    ax.set_facecolor("#fcfcfb")

    any_gap = False
    for slot, (bssid, points) in enumerate(by_bssid.items()):
        color = SERIES[slot]
        xs = [p[0] for p in points]
        seqs = series_seq[bssid]
        missed = gaps(seqs)

        # The connecting line, drawn per segment: red where the sequence number
        # skipped, recessive grey where it did not.
        segments = list(zip(zip(xs, seqs), zip(xs[1:], seqs[1:])))
        if segments:
            colors = [CRITICAL if m else color for m in missed[1:]]
            widths = [2.0 if m else 1.0 for m in missed[1:]]
            any_gap |= any(missed)
            ax.add_collection(
                LineCollection(segments, colors=colors, linewidths=widths, alpha=0.8, zorder=1)
            )

        for flow, marker in MARKERS.items():
            sel = [(x, s) for (x, _s, f), s in zip(points, seqs) if f == flow]
            if sel:
                ax.plot(
                    [p[0] for p in sel], [p[1] for p in sel],
                    marker, color=color, markersize=5, linestyle="none",
                    markeredgecolor="#fcfcfb", markeredgewidth=0.5, zorder=2,
                )

    ax.set_xlabel(xlabel, color=MUTED)
    ax.set_ylabel(ylabel, color=MUTED)
    ax.set_title(title, color=INK, loc="left", fontsize=13)
    ax.grid(True, color=GRID, linewidth=0.8, zorder=0)
    ax.set_axisbelow(True)
    ax.tick_params(colors=MUTED)
    for side, spine in ax.spines.items():
        spine.set_visible(side in ("left", "bottom"))
        spine.set_color("#c3c2b7")

    handles = []
    if len(by_bssid) > 1:  # a single BSSID is named in the title instead
        handles += [
            Line2D([], [], color=SERIES[i], marker="o", linestyle="none", label=f"BSSID {b}")
            for i, b in enumerate(by_bssid)
        ]
    handles += [
        Line2D([], [], color=MUTED, marker=m, linestyle="none", label=f"flow {f}")
        for f, m in MARKERS.items()
    ]
    if any_gap:
        handles.append(Line2D([], [], color=CRITICAL, linewidth=2, label="frames missed"))
    ax.legend(handles=handles, frameon=False, labelcolor=MUTED, loc="upper left")

    if out:
        fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("csv", nargs="?", type=Path, default=Path("halow_switch.csv"),
                        help="CSV written by the halow_switch binary")
    parser.add_argument("--out", type=Path,
                        help="save to this file instead of opening a window")
    parser.add_argument("--time", choices=["relative", "epoch", "elapsed"], default="relative",
                        help="x axis: seconds since the first frame (default), raw UNIX epoch, "
                             "or the logger's monotonic elapsed_s column")
    parser.add_argument("--raw-seq", action="store_true",
                        help="plot the 12-bit field as-is instead of unwrapping its rollover")
    args = parser.parse_args()

    if not args.csv.exists():
        sys.exit(f"missing {args.csv} — run halow_switch first")

    by_bssid, skipped = load(args.csv, args.time)
    if len(by_bssid) > len(SERIES):
        sys.exit(f"{len(by_bssid)} BSSIDs but only {len(SERIES)} palette slots — filter the capture")

    series_seq = {
        bssid: [p[1] for p in points] if args.raw_seq else unwrap([p[1] for p in points])
        for bssid, points in by_bssid.items()
    }

    summarise(by_bssid, series_seq, skipped)

    xlabel = {
        "relative": "time since first frame (s)",
        "epoch": "UNIX epoch (s)",
        "elapsed": "elapsed since log opened (s)",
    }[args.time]
    ylabel = "802.11 sequence number" + ("" if args.raw_seq else " (unwrapped)")
    title = f"{args.csv.name} — receive time vs sequence number"
    if len(by_bssid) == 1:
        title += f", BSSID {next(iter(by_bssid))}"
    plot(by_bssid, series_seq, xlabel, ylabel, title, args.out)


if __name__ == "__main__":
    main()
