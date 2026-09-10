#!/usr/bin/env python3
"""Compare packet error rate against transmit IFS across the captures in `../csv`.

One point per window of `--window` transmitted packets: its packet error rate
against the inter-frame spacing the transmitter was using at the time. Plotted
that way the captures line up on a common axis — a capture is a sweep, not a
timeline, so time on x only ever showed which run started first.

Colour is the PHY and only the PHY — blue for 802.11ah, orange for 802.15.4 —
so the same hue means the same radio in every figure. The marker and dash are
the capture, which is what separates two captures on one PHY, and what keeps
them apart once a journal prints the figure in greyscale.

PER needs a count of what was sent, and the captures stamp that two ways, so
the loader picks per file — and per PHY, for the files carrying both:

  * **stamped** — the `step` and `run` columns the Zigbee firmware writes. The
    transmitted-packet index is `run * (max_step + 1) + step`: exact, with
    nothing inferred, so no frame has to be second-guessed.
  * **sequence** — a `seq` column and nothing else: the 802.11 12-bit sequence
    control, or the Zigbee 8-bit DSN. A transmitter counts monotonically, so
    gaps in it are losses — but only after frames from *other* transmitters are
    dropped, which `plot_halow_swap.py` found could otherwise turn a 0.1% PER
    into 2.97%.

The IFS comes the same two ways. A `wait_us` or `ifs_us` column is the
transmitter's own figure and is used as-is; a capture without one has its IFS
measured, as the median per-packet spacing inside the window — the gap between
two receptions divided by how many packets apart they are, so a loss stretches
no estimate.

Usage:
    python3 plot_per_compare.py [--preset ziglow|halow|zigbee] [--window 1000]
                                [--select halow_swap ...] [--out plot.png]

With no arguments it opens a window with every capture listed, one tickable
checkbox each, and a box for the packets-per-window.
"""

import argparse
import csv
import re
import statistics
import sys
from bisect import bisect_left
from collections import Counter, defaultdict
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.lines import Line2D
from matplotlib.ticker import FuncFormatter, NullFormatter
from matplotlib.widgets import Button, CheckButtons, TextBox

# Sequence-number recovery. The only two fields in these captures are the
# 802.11 sequence control (12 bits) and the Zigbee DSN (8 bits), so the modulus
# is one of two values and is chosen by whether the field ever exceeds 255.
SEQ_MODULO_LARGE = 4096
SEQ_MODULO_SMALL = 256
NEIGHBOUR_TOL = 16  # frames either side of an intruder land within this many counts

# Time columns, most preferred first. `rx_t_ms` is milliseconds since the log
# opened; the other two are already seconds.
TIME_COLUMNS = (("rx_t_ms", 1e-3), ("elapsed_s", 1.0), ("rx_epoch_s", 1.0))
IFS_COLUMNS = ("wait_us", "ifs_us")

# Measured IFS is grouped into 100 µs bins to draw the trend line through it —
# finer than that and the "trend" is just the scatter redrawn. Only measured
# values are binned; a stamped IFS is exact and is grouped by its own value, so
# this never merges two steps of a sweep.
IFS_BIN_MS = 0.1

# Colour is the PHY and nothing else — two hues, fixed, so every figure reads
# the same way and a reader never has to check which blue meant what. The pair
# clears CVD separation and 3:1 contrast against white.
PHY_COLOUR = {"802.11ah": "#2a78d6", "802.15.4": "#eb6834", None: "#6b6a66"}

# The capture carries the shape, assigned alphabetically so a capture keeps its
# marker no matter which others are ticked. Dash is the second axis of that
# encoding, which is what separates two captures on the same PHY in greyscale.
MARKERS = ["o", "s", "^", "D", "v", "P", "X", "*"]
# Solid, long dash, dotted — three styles that stay distinct in greyscale and
# at journal column width. The middle one is deliberately long: a short dash
# reads as a dotted line once a figure is scaled down to one column.
DASHES = [(0, ()), (0, (9, 3)), (0, (1, 1.6))]
MAX_FILES = len(MARKERS) * len(DASHES)

# The sweep an unstamped capture was recorded under. A stamped capture carries
# its own and ignores this entirely; an unstamped one has only packet ids, so
# the run index is `(id - first id) // run_size` and the IFS follows from the
# schedule. Overridden by --run-size / --ifs-start / --ifs-step.
SCHEDULE = {"run_size": 1000, "ifs_start": 10.0, "ifs_step": -0.1}

# Set when --run-size is given explicitly. `stamped_ids` normally infers the
# packets-per-run from the largest step it sees, which is right until a capture
# carries a few frames stamped past the end of a run: ziglow_swap_aaaaaaaaaaaaaa
# sends 100 per step but has 31 frames stamped 100 or 101, so the inference
# reads 102 and inflates every sent-count by 2%. An explicit value wins.
RUN_SIZE_OVERRIDE = None
# Per-capture overrides, `{stem: packets}`. Captures in one figure need not
# share a run size — ziglow_swap_aaaaaaaaaaaaaa sends 100 per step where
# ziglow_swap_FPGA sends 50 — so a single global value cannot serve a figure
# that mixes them.
RUN_SIZE_BY_STEM = {}


def run_size_for(stem):
    """Packets per IFS step for one capture: its own override, else global."""
    return RUN_SIZE_BY_STEM.get(stem) or RUN_SIZE_OVERRIDE

# Set by --merge-phy: report a dual-PHY capture as one pooled trace.
MERGE_PHY = False

# Set by --overlay: pre-aggregated PER curves drawn alongside the captures.
#
# A capture is a list of received frames, and this script's whole loading path
# exists to turn that into a PER — recovering what was sent from sequence
# numbers or from a schedule. A replay sweep has already done that arithmetic
# and done it exactly: `recording/bench/run_sweep.py` knows how many frames it
# generated, so its PER is a count, not a reconstruction. Pushing it back
# through `Trace` would mean inventing per-frame ids for it just so they could
# be counted again. It is drawn from its own points instead.
OVERLAYS = []

# `phy` code in a replay results CSV -> the name the colour table is keyed on.
OVERLAY_PHY = {"H": "802.11ah", "Z": "802.15.4"}

# Set by --overlay-split: keep an overlay's PHYs apart even when their curves
# are identical and one would be drawn entirely on top of the other.
OVERLAY_SPLIT = False

# Set by --overlay-pool: collapse an overlay's PHYs into a single curve whether
# or not they coincide. The auto-collapse below only fires on exact equality,
# which a single frame's difference at one IFS step is enough to defeat.
OVERLAY_POOL = False

SURFACE = "#fcfcfb"
MUTED = "#898781"
GRID = "#e1e0d9"
INK = "#0b0b0b"
FAINT = "#c3c2b7"

# `--paper` restyles for a journal figure rather than a screen: Latin Modern
# Roman is the Computer Modern LaTeX sets body text in, so the figure's text
# matches the page it lands on. Text is kept as text in the SVG rather than
# converted to outlines, which keeps it selectable and re-styleable — the font
# has to be installed wherever the file is opened, and it is what LaTeX uses.
PAPER_RC = {
    "font.family": "serif",
    "font.serif": ["Latin Modern Roman", "Nimbus Roman", "Times New Roman",
                   "DejaVu Serif"],
    "mathtext.fontset": "cm",
    "font.size": 9,
    "axes.labelsize": 9,
    "axes.titlesize": 9,
    "legend.fontsize": 8,
    "xtick.labelsize": 8,
    "ytick.labelsize": 8,
    "axes.linewidth": 0.6,
    "xtick.direction": "in",
    "ytick.direction": "in",
    "xtick.top": True,
    "ytick.right": True,
    "xtick.major.width": 0.6,
    "ytick.major.width": 0.6,
    "xtick.minor.width": 0.4,
    "ytick.minor.width": 0.4,
    "lines.markeredgewidth": 0.4,
    "svg.fonttype": "none",
    "pdf.fonttype": 42,  # TrueType, not Type 3 — most journals reject Type 3
    "ps.fonttype": 42,
}

# The three comparisons this script was written for, by file stem.
#
# Order matters: it sets the line style, first solid then dashed then dotted
# (see `style`). Quick tune leads because it is the result these figures are
# about, and a solid line is the one a reader follows first.
PRESETS = {
    "interband": ("ziglow_swap_quicktune", "ziglow_swap_FPGA", "ziglow_swap_soapy"),
    "inband": ("zigbee_swap_quicktune", "zigbee_swap_2chan_FPGA", "zigbee_swap2ZIG"),
    "soft": ("halow_switchv2mcs0", "halow_switchv2mcs4", "zigbee_swap"),
}


# ---------------------------------------------------------------- loading


class Trace:
    """One comparable series: packet ids, arrival times and IFS, all aligned."""

    def __init__(self, file, phy, ids, times, ifs_us, note, step_size=None):
        self.file = file
        self.phy = phy  # "802.11ah", "802.15.4", or None
        self.ids = ids
        self.times = times  # seconds since this capture's first frame
        self.ifs_us = ifs_us  # transmitter's own figure per frame, or None
        # Packets the transmitter sent per IFS step, when the ids were
        # synthesised from the schedule rather than read off the frames.
        # Without it `_window_group` measures a step against the ids it can
        # see, which for a schedule-derived trace are numbered consecutively —
        # so every step reads received == expected and the curve sits flat at
        # 0% while the overall PER is correctly non-zero.
        self.step_size = step_size
        self.note = note

    @property
    def label(self):
        return self.file if self.phy is None else f"{self.file} {self.phy}"

    @property
    def expected(self):
        return self.ids[-1] - self.ids[0] + 1

    @property
    def overall(self):
        return 100.0 * (1 - len(self.ids) / self.expected)


def circ(a, b, modulo):
    """Forward distance from `b` to `a` around a sequence field of `modulo`."""
    return (a - b) % modulo


def drop_intruders(seqs, keep_idx, modulo):
    """Drop frames from other transmitters, judged locally.

    A frame is not ours when deleting it makes its two neighbours continuous:
    its own step is a big jump, but the step straight from the previous frame
    to the next one is small. Judging each frame against a running cursor
    instead lets one intruder poison the cursor and reject everything after it.
    """
    kept, dropped = [], 0
    last = len(keep_idx) - 1
    for i, idx in enumerate(keep_idx):
        if 0 < i < last:
            own = circ(seqs[i], seqs[i - 1], modulo)
            bridge = circ(seqs[i + 1], seqs[i - 1], modulo)
            suspect = own > NEIGHBOUR_TOL and bridge <= NEIGHBOUR_TOL
        elif last > 0:
            # The two endpoints have only one neighbour, so the bridge test
            # cannot run and they were once accepted unconditionally. A foreign
            # frame there is the worst case there is: it is the first or last
            # id, so it sets an end of the span and inflates every count taken
            # against it — one such frame at the tail of a ziglow capture read
            # as 27.85% PER where the truth was 4.90%. Judge them one-sidedly
            # instead: an endpoint that sits far from its only neighbour is
            # dropped. The asymmetry is deliberate — being wrong costs one
            # frame off an end, while keeping an intruder costs the whole span.
            near = seqs[1] if i == 0 else seqs[last - 1]
            step = circ(near, seqs[i], modulo) if i == 0 else circ(seqs[i], near, modulo)
            suspect = step > NEIGHBOUR_TOL
        else:
            suspect = False
        if suspect:
            dropped += 1
            continue
        kept.append((seqs[i], idx))
    return kept, dropped


def sequence_ids(seqs, keep_idx):
    """Monotonic packet ids from a wrapping sequence field.

    Returns `(ids, row_indices, note)`. A negative step of more than half the
    modulus is a wrap and is unwrapped; anything else implausible is a
    corrupted or foreign sequence number, and that frame is rejected rather
    than unwrapped — unwrapping it would fabricate hundreds of "lost" packets.
    """
    modulo = SEQ_MODULO_LARGE if max(seqs) > 255 else SEQ_MODULO_SMALL
    max_gap = max(modulo // 8, 64)

    pairs, intruders = drop_intruders(seqs, keep_idx, modulo)
    ids, rows, rejected, offset = [], [], 0, 0
    for seq, idx in pairs:
        if not ids:
            ids.append(seq)
            rows.append(idx)
            continue
        cand = seq + offset
        if cand < ids[-1] - modulo // 2:  # wrapped
            offset += modulo
            cand = seq + offset
        step = cand - ids[-1]
        if step <= 0 or step > max_gap:
            rejected += 1
            continue
        ids.append(cand)
        rows.append(idx)

    note = f"{modulo}-count sequence field"
    if intruders or rejected:
        note += f", {intruders + rejected} frame(s) dropped as not this transmitter's"
    return ids, rows, note


def stamped_ids(steps, runs, keep_idx, run_size=None):
    """Monotonic packet ids from the firmware's `step` and `run` stamps.

    A frame that decoded without a readable stamp carries `step = run = -1`.
    There is no way to place it in the transmitted order, so it is dropped
    rather than guessed at — in these captures it is a handful of frames in
    tens of thousands.
    """
    period = run_size or (max(steps) + 1)
    if period < 2:
        return [], [], "only one step stamped"

    ids, rows, unstamped, backwards, overrun = [], [], 0, 0, 0
    for step, run, idx in zip(steps, runs, keep_idx):
        if step < 0 or run < 0:
            unstamped += 1
            continue
        if step >= period:
            overrun += 1
            continue
        pid = run * period + step
        if ids and pid <= ids[-1]:
            backwards += 1
            continue
        ids.append(pid)
        rows.append(idx)

    note = f"firmware stamp, {period} packets per run"
    if unstamped:
        note += f", {unstamped} unstamped frame(s) dropped"
    if backwards:
        note += f", {backwards} out-of-order frame(s) dropped"
    if overrun:
        note += f", {overrun} frame(s) stamped past the run dropped"
    return ids, rows, note


def schedule_ids(kept_ifs, keep_idx, per_step=None):
    """Ids from the transmit schedule, for a group with no usable numbering.

    A ziglow capture stamps `step`/`run` on its Zigbee frames only, so the
    HaLow half has nothing but the 802.11 sequence control — and that is not
    always usable: in ziglow_swap_aaaaaaaaaaaaaa 93% of HaLow frames carry
    seq 16, so `sequence_ids` rejects nearly all of them and the trace is lost
    entirely rather than plotted.

    What is still known is the schedule. Every frame inherits an IFS (forward
    filled from the Zigbee stamps), the IFS identifies the step, and the step
    was sent `run_size` packets long. Numbering the arrivals within each step
    therefore places them: the frames that never arrived show up as the gap
    between a step's count and its size, which is exactly the PER. It cannot
    say *which* packet was lost, only how many — enough for a rate, and the
    only thing recoverable once the transmitter's own numbering is gone.
    """
    run_size = max(per_step or SCHEDULE["run_size"], 1)
    start, step_ms = SCHEDULE["ifs_start"], abs(SCHEDULE["ifs_step"]) or 0.1
    ids, rows, counts, dropped = [], [], {}, 0
    for ifs_us, idx in zip(kept_ifs, keep_idx):
        if not ifs_us:
            dropped += 1
            continue
        s_i = int(round((start - ifs_us / 1000.0) / step_ms))
        if s_i < 0:
            dropped += 1
            continue
        n = counts.get(s_i, 0)
        if n >= run_size:      # more arrivals than the step can hold
            dropped += 1
            continue
        counts[s_i] = n + 1
        pid = s_i * run_size + n
        if ids and pid <= ids[-1]:
            dropped += 1
            continue
        ids.append(pid)
        rows.append(idx)
    note = f"transmit schedule, {run_size} packets per step"
    if dropped:
        note += f", {dropped} frame(s) unplaceable"
    return ids, rows, note, run_size


def column(header, *names):
    """Index of the first of `names` present in `header`, or None."""
    for name in names:
        if name in header:
            return header.index(name)
    return None


def read_csv(path):
    """`(header, rows)`, or `(None, [])` when the file is not a CSV table."""
    with path.open(newline="", errors="replace") as handle:
        reader = csv.reader(handle)
        try:
            header = next(reader)
        except StopIteration:
            return None, []
        return header, [row for row in reader if row]


def phy_of(stem, tag):
    """Which PHY a trace is, for its marker.

    A `phy` column says so outright. Otherwise it comes from the file's name,
    which every capture here follows — corroborated by the schema, since the
    HaLow logs carry `frag`/`rftap_hex` and the Zigbee ones `step`/`run`/`tag`.
    """
    if tag in ("H", "h"):
        return "802.11ah"
    if tag in ("Z", "z"):
        return "802.15.4"
    if tag is not None:
        return None
    lowered = stem.lower()
    if lowered.startswith("halow"):
        return "802.11ah"
    if lowered.startswith("zigbee"):
        return "802.15.4"
    return None


def stamp_ifs(header, rows):
    """`[(row, ifs_us or None)]`, the stamped IFS forward-filled down the file.

    A `ziglow` capture stamps the IFS on its Zigbee frames only; the HaLow
    frames it interleaves were sent at the same spacing, so they inherit the
    last stamp seen — the same rule `plot_ziglow.py` uses to give HaLow frames
    their step. Filling before the PHY split is the whole point, so it has to
    happen while the two are still in transmit order.
    """
    idx = column(header, *IFS_COLUMNS)
    if idx is None:
        return [(row, None) for row in rows]
    out, last = [], None
    for row in rows:
        try:
            value = int(row[idx])
        except (IndexError, ValueError):
            value = -1
        if value > 0:
            last = value
        out.append((row, last))
    return out


def split_by_phy(header, records):
    """`[(tag, records)]` — split only where one file carries two PHYs.

    A `phy` column means the capture interleaves independent transmitters with
    separate id spaces, which have to be measured apart. `phy_active` is the
    receiver's listening flow over one transmitter's stream and is not split.
    """
    idx = column(header, "phy")
    if idx is None:
        return [(None, records)]
    groups = defaultdict(list)
    for row, ifs in records:
        if idx < len(row):
            groups[row[idx]].append((row, ifs))
    if len(groups) < 2:
        return [(None, records)]
    return [(tag, group) for tag, group in sorted(groups.items()) if len(group) > 1]


def build_trace(stem, tag, header, records):
    """A `Trace` from one homogeneous group of `(row, ifs_us)`, or `(None, reason)`."""
    t_col = next(((column(header, name), scale)
                  for name, scale in TIME_COLUMNS if name in header), None)
    if t_col is None:
        return None, "no time column, so no IFS to place it against"
    t_idx, t_scale = t_col

    event = column(header, "frame_event")
    step_idx, run_idx = column(header, "step"), column(header, "run")
    seq_idx = column(header, "seq")

    times, steps, runs, seqs, ifs, keep = [], [], [], [], [], []
    malformed = 0
    step_size = None
    for row, stamped in records:
        if event is not None and event < len(row) and row[event] != "rx":
            continue
        try:
            t = float(row[t_idx]) * t_scale
            step = int(row[step_idx]) if step_idx is not None else -1
            run = int(row[run_idx]) if run_idx is not None else -1
            seq = int(row[seq_idx]) if seq_idx is not None else -1
        except (IndexError, ValueError):
            malformed += 1
            continue
        keep.append(len(times))
        times.append(t)
        steps.append(step)
        runs.append(run)
        seqs.append(seq)
        ifs.append(stamped)

    if len(keep) < 2:
        return None, "fewer than two received frames"

    # With the PHYs merged there is no single numbering to read: one half is
    # stamped and the other carries a sequence field, in separate id spaces.
    # What is shared is the schedule — each step sent `run_size` packets per
    # PHY — so pool them and size the step by however many PHYs are present.
    own = run_size_for(stem)
    # The stamp is authoritative wherever it is present; the sequence field is
    # the fallback, and has to be defended against other transmitters.
    if step_idx is not None and run_idx is not None and any(s >= 0 for s in steps):
        ids, rows_kept, note = stamped_ids(steps, runs, keep, own)
    elif seq_idx is not None and max(seqs) >= 0:
        ids, rows_kept, note = sequence_ids(seqs, keep)
        # A sequence field that barely moves is not this transmitter counting;
        # fall back to the schedule rather than discarding the whole trace.
        if len(ids) < len(keep) // 2 and any(ifs):
            ids, rows_kept, note, step_size = schedule_ids(ifs, keep, own)
    elif any(ifs):
        ids, rows_kept, note, step_size = schedule_ids(ifs, keep, own)
    else:
        return None, "no step/run stamp, no sequence number and no IFS"

    if len(ids) < 2:
        return None, note or "no usable packet index"
    if malformed:
        note += f", {malformed} malformed row(s)"

    kept_ifs = [ifs[i] for i in rows_kept]
    note += ", IFS " + (
        "stamped" if any(v for v in kept_ifs)
        else f"from schedule ({SCHEDULE['ifs_start']:g} ms, "
             f"{SCHEDULE['ifs_step']:+g} ms per {SCHEDULE['run_size']} packets)"
    )
    t0 = times[rows_kept[0]]
    trace = Trace(stem, phy_of(stem, tag), ids,
                  [times[i] - t0 for i in rows_kept], kept_ifs, note, step_size)
    return trace, None


# The `halow_switch` log is not a table at all — it is the raw tap dump,
# `[tap <name>] Blob([byte, byte, ...])` per line. The 802.11 sequence control
# is recoverable from those bytes, but the dump carries no timestamp, so there
# is no IFS to place the capture against. It is read anyway so its PER can be
# reported rather than the file silently disappearing.
BLOB = re.compile(r"Blob\(\[([0-9,\s]+)\]\)")


def read_tap_dump(path):
    """`(received, expected)` from a raw tap dump, or None if it is not one."""
    seqs = []
    with path.open(errors="replace") as handle:
        for line in handle:
            match = BLOB.search(line)
            if not match:
                continue
            frame = bytes(int(b) for b in match.group(1).split(","))
            if len(frame) < 24 or (frame[0] >> 2) & 0x3 == 1:  # control: no seq
                continue
            seqs.append(int.from_bytes(frame[22:24], "little") >> 4)
    if len(seqs) < 2:
        return None
    ids, rows, note = sequence_ids(seqs, list(range(len(seqs))))
    return (len(ids), ids[-1] - ids[0] + 1) if len(ids) >= 2 else None


def load_dir(csv_dir):
    """`([Trace], [(name, reason)])` for every CSV in `csv_dir`, sorted by name."""
    traces, skipped = [], []
    for path in sorted(csv_dir.glob("*.csv")):
        header, rows = read_csv(path)
        if header is None or not rows:
            skipped.append((path.stem, "no rows"))
            continue
        if column(header, *(n for n, _s in TIME_COLUMNS)) is None:
            dump = read_tap_dump(path)
            if dump:
                received, expected = dump
                skipped.append((path.stem,
                                f"raw tap dump, no timestamps — PER is "
                                f"{100 * (1 - received / expected):.2f}% "
                                f"({received}/{expected}) but there is no IFS "
                                f"to plot it against"))
            else:
                skipped.append((path.stem, "not a capture table"))
            continue

        records = stamp_ifs(header, rows)
        for tag, group in split_by_phy(header, records):
            trace, reason = build_trace(path.stem, tag, header, group)
            if trace is None:
                name = path.stem if tag is None else f"{path.stem} [{tag}]"
                skipped.append((name, reason))
            else:
                traces.append(trace)
    if MERGE_PHY:
        # Pool the halves of every capture that produced more than one.
        by_file = defaultdict(list)
        for t in traces:
            by_file[t.file].append(t)
        traces = [parts[0] if len(parts) == 1 else MergedTrace(parts)
                  for parts in by_file.values()]
    return traces, skipped


# ---------------------------------------------------------------- analysis


def window_ifs(trace, rows, lo, hi):
    """`(ifs_ms, straddles_a_step)` for one window, or `(None, False)`.

    The transmitter's own figure when it stamped one — the value it spent most
    of the window at, not the median, because a window wider than the sweep's
    step covers two of them and the median between two steps is a spacing the
    transmitter never used. Such a window is flagged so the count can be
    reported; the fix is a window no wider than one step.

    With nothing stamped the IFS is measured, as the median per-packet spacing:
    each gap between two receptions divided by how many packets apart they are,
    so a lost packet widens no estimate.
    """
    stamped = Counter(trace.ifs_us[i] for i in rows if trace.ifs_us[i])
    if stamped:
        return stamped.most_common(1)[0][0] / 1000.0, len(stamped) > 1

    ids, times = trace.ids, trace.times
    spacings = [(times[i + 1] - times[i]) * 1000.0 / (ids[i + 1] - ids[i])
                for i in range(max(lo, 0), min(hi, len(ids) - 1))]
    return (statistics.median(spacings) if spacings else None), False


def _window_group(trace, members, window):
    """Window one group of frame indices over the packets it spans.

    `[(ifs_ms, per_pct, received, expected, straddles)]`. Windows are anchored
    at the group's first packet rather than at packet 0, so a receiver that
    started late is not charged for the frames it was never up for; the last
    one is measured against the last packet seen, for the same reason. A window
    in which nothing decoded is still reported, at 100%, with its IFS taken
    from the frames either side.
    """
    ids = trace.ids
    base, last = ids[members[0]], ids[members[-1]]
    if trace.step_size:
        # The step is known to be `step_size` packets long whatever arrived.
        last = base + trace.step_size - 1
    buckets = defaultdict(list)
    for i in members:
        buckets[(ids[i] - base) // window].append(i)

    out = []
    for key in range((last - base) // window + 1):
        rows = buckets.get(key, [])
        start = base + key * window
        expected = min(window, last - start + 1)
        if expected < 1:
            continue
        if rows:
            ifs, straddles = window_ifs(trace, rows, rows[0], rows[-1])
        else:
            # Nothing decoded here, so the window borrows the frames bracketing
            # it — the first and last windows always hold one, so they exist.
            hi = bisect_left(ids, start)
            ifs, straddles = window_ifs(
                trace, [max(hi - 1, 0), min(hi, len(ids) - 1)], hi - 1, hi + 1)
        if ifs is None:
            continue
        out.append((ifs, 100.0 * (1 - len(rows) / expected),
                    len(rows), expected, straddles))
    return out


def _steps_by_silence(trace, factor=8.0, local=25, min_len=20):
    """Packet indices grouped into transmitter steps, cut at each silence.

    An unstamped capture carries no run index, so the obvious partition is by
    packet count — but a sweep's steps and a fixed packet count do not line up,
    and a window straddling two steps measures an IFS between them and a PER
    that mixes both. That is what put the scatter into the unstamped series.

    The transmitter pauses between steps for longer than the spacing it uses
    within one, so the boundary is visible in the arrival times. Two details
    make the test survive real captures:

      * Spacing is measured **per transmitted packet** — the gap divided by how
        many packet ids apart the two receptions are — so a lost packet does
        not look like a pause.
      * The threshold follows a **local** median of the preceding spacings
        rather than one figure for the whole capture. A sweep from 10 ms down
        to 1 ms has no single normal spacing, and a global threshold would find
        every boundary at the slow end and none at the fast end.

    `factor` is set from the captures rather than guessed: in
    `halow_switchv2mcs0` the per-packet spacing has a median of 6.8 ms and a
    99th percentile of 12 ms, while the ten largest are 144-154 ms — the
    silences stand well clear of everything else. Factor 8 finds 94 boundaries
    where the sweep has 91 steps; 2.5 finds 292, cutting on ordinary jitter.

    Fragments shorter than `min_len` are folded back into the preceding step. A
    handful of frames is not a step, and its measured IFS would be noise: the
    smallest spacings in these captures are ~0.03 ms, far below any IFS the
    transmitter uses, so a fragment around one of those would otherwise plant a
    point near zero on the axis.
    """
    n = len(trace.ids)
    if n < 3:
        return [list(range(n))]

    spacing = []
    for i in range(n - 1):
        packets = max(trace.ids[i + 1] - trace.ids[i], 1)
        spacing.append((trace.times[i + 1] - trace.times[i]) / packets)

    steps, cur = [], [0]
    for i, gap in enumerate(spacing):
        window_start = max(0, i - local)
        recent = [g for g in spacing[window_start:i] if g > 0]
        median = statistics.median(recent) if recent else 0.0
        if median > 0 and gap > factor * median:
            steps.append(cur)
            cur = []
        cur.append(i + 1)
    steps.append(cur)

    merged = []
    for step in steps:
        if merged and len(step) < min_len:
            merged[-1].extend(step)
        elif len(step) >= 2:
            merged.append(step)
    return merged or [list(range(n))]


class MergedTrace:
    """Two PHYs of one capture reported as a single pooled series.

    Merging cannot be done on the packet ids: the halves live in separate id
    spaces (one stamped, one a sequence field), and synthesising a shared one
    from the schedule only works for a capture whose sweep matches the
    configured one — ziglow_swap_FPGA runs 10-200 ms ascending, so every step
    index came out negative and the trace collapsed to 50 frames.

    The windows are already commensurate, though: each carries a received and
    an expected count against a real IFS. Summing those per IFS pools the PHYs
    without either half having to know how the other was numbered.
    """

    def __init__(self, parts):
        self.parts = parts
        self.file = parts[0].file
        self.phy = None
        self.ids = [i for p in parts for i in p.ids]
        self.times = [t for p in parts for t in p.times]
        self.ifs_us = [v for p in parts for v in p.ifs_us]
        self.step_size = None
        self.note = f"{len(parts)} PHYs pooled, " + "; ".join(
            f"{p.phy}: {p.note}" for p in parts)
        self._recv = sum(len(p.ids) for p in parts)
        self._sent = sum(p.expected for p in parts)

    @property
    def label(self):
        return self.file

    @property
    def expected(self):
        return self._sent

    @property
    def overall(self):
        return 100.0 * (1 - self._recv / self._sent) if self._sent else 0.0


def pool_windows(series_list):
    """Sum received/expected across PHYs at each IFS."""
    totals = defaultdict(lambda: [0, 0, False])
    for series in series_list:
        for ifs, _per, recv, expected, straddles in series:
            slot = totals[ifs]
            slot[0] += recv
            slot[1] += expected
            slot[2] = slot[2] or straddles
    out = []
    for ifs in sorted(totals):
        recv, expected, straddles = totals[ifs]
        if expected < 1:
            continue
        out.append((ifs, 100.0 * (1 - recv / expected), recv, expected, straddles))
    return out


def windowed_per(trace, window):
    """`[(ifs_ms, per_pct, received, expected, straddles)]` per packet window.

    Where the IFS is stamped, the packets are partitioned by it *before* being
    windowed, so a window can never span two steps of a sweep. It is not a
    refinement: a window wider than one step used to take the mode of the
    values inside it, and a window covering exactly two runs is a tie the mode
    breaks arbitrarily — which silently kept half the sweep and dropped the
    other half, differently for each PHY. Partitioning first means every step
    the transmitter used gets its own point, whatever `window` is set to, and
    `window` means what it says within a step: packets per point, capped by the
    run if the run is shorter.

    With nothing stamped the partition comes from the *schedule*, via
    `--run-size` / `--ifs-start` / `--ifs-step`: run index is
    `(packet id - first id) // run_size` and its IFS follows from the sweep.

    Timing cannot supply the IFS. What the arrival times measure is the frame
    *rate* — the inter-frame spacing plus the time the frame itself occupies —
    so the estimate sits a whole frame length above the IFS the transmitter was
    configured with (measured: 10.87 ms where the transmitter used 10.0). Packet
    ids do not have that problem: the sequence field advances once per
    transmitted frame regardless of how long a frame takes.
    """
    if isinstance(trace, MergedTrace):
        return pool_windows([windowed_per(p, window) for p in trace.parts])

    if window < 2 or len(trace.ids) < 2:
        return []

    if not any(v for v in trace.ifs_us):
        base = trace.ids[0]
        groups = defaultdict(list)
        for i, pid in enumerate(trace.ids):
            groups[(pid - base) // SCHEDULE["run_size"]].append(i)
        out = []
        for run in sorted(groups):
            ifs = SCHEDULE["ifs_start"] + run * SCHEDULE["ifs_step"]
            if ifs <= 0:
                continue
            out += [(ifs,) + point[1:]
                    for point in _window_group(trace, groups[run], window)]
        return sorted(out)

    groups = defaultdict(list)
    for i, ifs in enumerate(trace.ifs_us):
        groups[ifs].append(i)
    out = []
    for ifs in sorted(groups, key=lambda v: (v is None, v)):
        out += _window_group(trace, groups[ifs], window)
    return sorted(out)


def aggregate(series, stamped):
    """`[(ifs_ms, per_pct)]` — the trend through windows that share an IFS.

    Stamped IFS values are grouped exactly; so are the scheduled ones, since
    both come from a fixed grid. Only genuinely measured spacings need binning,
    because no two windows ever measure quite the same value.
    """
    pooled = defaultdict(lambda: [0, 0])
    for point in series:
        key = point[0] if stamped else round(point[0] / IFS_BIN_MS) * IFS_BIN_MS
        pooled[key][0] += point[2]
        pooled[key][1] += point[3]
    return [(k, 100.0 * (1 - r / e)) for k, (r, e) in sorted(pooled.items()) if e]


def style(slot, paper=False):
    """`(marker, dash)` for a capture, fixed to its slot so nothing repaints.

    On screen the dash only separates captures past the eighth, where the
    shapes run out. On paper every capture gets its own dash as well: colour
    there is the PHY, so two captures on the same PHY have only shape and dash
    to tell them apart, and a journal figure has to survive greyscale.
    """
    dash = DASHES[slot % len(DASHES)] if paper else DASHES[slot // len(MARKERS)]
    return MARKERS[slot % len(MARKERS)], dash


def summarise(selected, window):
    """The numbers behind the plot, as a table — several series sit below the
    3:1 contrast line against the surface, so they are never identified by
    colour alone."""
    print(f"\n== PER over windows of {window} transmitted packets")
    print(f"{'capture':<26}{'received':>10}{'sent':>9}{'PER':>8}"
          f"{'IFS ms':>16}  notes")
    for trace in selected:
        series = windowed_per(trace, window)
        span = (f"{min(p[0] for p in series):6.2f}–{max(p[0] for p in series):<6.2f}"
                if series else " " * 13)
        note = trace.note
        straddled = sum(1 for p in series if p[4])
        if straddled:
            note += (f", {straddled} of {len(series)} window(s) straddle an IFS "
                     f"step — shorten the window to separate them")
        print(f"{trace.label:<26}{len(trace.ids):>10}{trace.expected:>9}"
              f"{trace.overall:>7.2f}%  {span}  {note}")


def load_overlay(spec):
    """Read one `--overlay [NAME=]PATH` into `(name, [(phy, [(ifs, per)])])`.

    Two shapes are accepted, because both are natural to write:

      wide  `ifs_ms, per_H_pct, per_Z_pct`  — what run_sweep.py emits
      long  `ifs_ms, phy, per_pct`          — one row per PHY per step

    Rows with a blank or non-numeric PER are skipped rather than plotted as
    zero: a step that failed to run is missing data, and a curve that dips to
    0% there would read as a step that ran perfectly.
    """
    name, _, path = spec.rpartition("=")
    path = Path(path)
    if not name:
        name = path.stem
    if not path.exists():
        sys.exit(f"--overlay: {path} not found")

    by_phy = defaultdict(list)
    with path.open(newline="") as fh:
        rows = list(csv.DictReader(fh))
    if not rows:
        sys.exit(f"--overlay: {path} has no rows")

    cols = rows[0].keys()

    # Pooling asked for explicitly. Done on the counts where the file carries
    # them (`rx_*` / `sent_*`), which is the honest pooled rate:
    #
    #     1 - (rx_H + rx_Z) / (sent_H + sent_Z)
    #
    # Averaging the two percentages would only agree with that when both PHYs
    # sent the same number of frames, and would quietly weight a short run the
    # same as a long one when they did not.
    if OVERLAY_POOL:
        rx_cols = [c for c in cols if c.startswith("rx_")]
        sent_cols = [c for c in cols if c.startswith("sent_")]
        pts = []
        for row in rows:
            ifs = float(row["ifs_ms"])
            if rx_cols and sent_cols:
                try:
                    rx = sum(float(row[c]) for c in rx_cols)
                    sent = sum(float(row[c]) for c in sent_cols)
                except (TypeError, ValueError):
                    continue
                if sent <= 0:
                    continue
                pts.append((ifs, 100.0 * (1 - rx / sent)))
            else:
                vals = []
                for k, v in row.items():
                    if k.startswith("per_") and k.endswith("_pct"):
                        try:
                            vals.append(float(v))
                        except (TypeError, ValueError):
                            pass
                if vals:
                    pts.append((ifs, sum(vals) / len(vals)))
        if not pts:
            sys.exit(f"--overlay: {path} had no poolable rows")
        print(f"overlay {name}: pooled "
              f"{'from counts' if rx_cols and sent_cols else 'as the mean of the PHY rates'}")
        return name, [(None, sorted(pts))]

    for row in rows:
        try:
            ifs = float(row["ifs_ms"])
        except (KeyError, TypeError, ValueError):
            sys.exit(f"--overlay: {path} needs an `ifs_ms` column")
        if "phy" in cols and "per_pct" in cols:
            pairs = [(row["phy"], row["per_pct"])]
        else:
            pairs = [(k.split("_")[1], v) for k, v in row.items()
                     if k.startswith("per_") and k.endswith("_pct")]
        for code, val in pairs:
            try:
                per = float(val)
            except (TypeError, ValueError):
                continue
            by_phy[OVERLAY_PHY.get(code, code)].append((ifs, per))

    # Sorted by IFS so the line is drawn along the axis rather than in the
    # order the sweep happened to run, which descends.
    series = [(phy, sorted(pts)) for phy, pts in sorted(by_phy.items()) if pts]

    # A strictly alternating replay loses both PHYs together — the receiver
    # swaps after every decoded frame, so a frame missed on one PHY leaves it
    # parked and costs the other the next one too. The two curves then come out
    # bit-identical, and drawing both hides one completely under the other:
    # the figure shows a single line in whichever colour was drawn last, which
    # reads as a result for that PHY alone. Pool them into one neutral-coloured
    # curve instead, which is what the numbers actually say.
    if len(series) > 1 and not OVERLAY_SPLIT:
        first = series[0][1]
        if all(pts == first for _, pts in series[1:]):
            print(f"overlay {name}: {', '.join(p for p, _ in series)} are identical "
                  f"— pooled into one curve (use --overlay-split to force both)")
            return name, [(None, first)]
    return name, series


# ---------------------------------------------------------------- drawing


def draw(axes, traces, slots, active, window, title, xlim=None,
         labels=None, paper=False, markers=None, logx=False,
         legend_loc=None, show_windows=True):
    """Redraw the panel and its legend for the currently ticked traces."""
    ax, ax_legend = axes
    ax.clear()
    if ax_legend is not None:
        ax_legend.clear()
        ax_legend.set_axis_off()
    labels = labels or {}
    markers = markers or {}

    if paper:
        # A journal figure is a boxed frame with ticks turned inward, no title
        # — the caption carries that — and a grid faint enough to read through.
        ax.grid(True, color="#d8d8d8", linewidth=0.4, linestyle=(0, (1, 2)))
        ax.minorticks_on()
        ax.tick_params(which="both", top=True, right=True)
        for spine in ax.spines.values():
            spine.set_visible(True)
            spine.set_color(INK)
        ax.set_xlabel("Transmit inter-frame spacing (ms)")
        ax.set_ylabel("Packet error rate (\\%)" if plt.rcParams["text.usetex"]
                      else "Packet error rate (%)")
    else:
        ax.set_facecolor(SURFACE)
        ax.grid(True, color=GRID, linewidth=0.8)
        ax.tick_params(colors=MUTED, labelsize=9)
        for side, spine in ax.spines.items():
            spine.set_visible(side in ("left", "bottom"))
            spine.set_color(FAINT)
        ax.set_xlabel("transmit IFS (ms)", color=MUTED)
        ax.set_ylabel("packet error rate (%)", color=MUTED)
        ax.set_title(f"{title} — {window} transmitted packets per window",
                     color=INK, loc="left", fontsize=13)
    ax.set_axisbelow(True)

    selected = [t for i, t in enumerate(traces) if active[i]]
    # An overlay is a complete curve on its own, so a figure of nothing but
    # overlays is a legitimate request — only an empty figure is not.
    if not selected and not OVERLAYS:
        ax.text(0.5, 0.5, "tick a capture on the left", color=MUTED,
                ha="center", va="center", transform=ax.transAxes)
        return

    width = 1.3 if paper else 2
    size = 4.5 if paper else 7
    # The screen surface is off-white; on a transparent journal figure that
    # would print as a visible ring around every marker.
    edge = "white" if paper else SURFACE
    handles = []
    for trace in selected:
        marker, dash = style(slots[trace.file], paper)
        # Slots are global, so two captures that sit next to each other in one
        # figure can land on shapes that read poorly together; `--marker` picks
        # the pair for that figure without disturbing the others.
        marker = markers.get(trace.file, marker)
        color = PHY_COLOUR[trace.phy]
        series = windowed_per(trace, window)
        if not series:
            continue
        # Every window as a faint mark, so the spread at each IFS stays visible,
        # and the trend through them as the line that carries the reading.
        # Once the windows line up one-per-step there is nothing left for them
        # to show — they sit under the line and only tint it — so they can be
        # turned off.
        if show_windows:
            ax.plot([p[0] for p in series], [p[1] for p in series],
                    marker, color=color, markersize=size * (0.4 if paper else 0.6),
                    linestyle="none", alpha=0.2 if paper else 0.28,
                    markeredgecolor="none", zorder=1)
        trend = aggregate(series, stamped=any(v for v in trace.ifs_us))
        # At a column's width a sweep has more steps than the axes has room for
        # markers, and a solid ribbon of them buries the line. Marking every
        # nth point keeps the shape as an identifier while the line carries the
        # reading — every window is still drawn, as the faint marks beneath.
        every = max(1, round(len(trend) / 12)) if paper else 1
        ax.plot([p[0] for p in trend], [p[1] for p in trend],
                color=color, linewidth=width, linestyle=dash, marker=marker,
                markersize=size, markeredgecolor=edge, markeredgewidth=0.5,
                markevery=every, zorder=2)
        name = labels.get(trace.file, trace.file)
        if trace.phy and name == trace.file:
            name = f"{name} {trace.phy}"
        elif trace.phy and any(t.file == trace.file and t.phy != trace.phy
                               for t in selected):
            name = f"{name}, {trace.phy}"
        handles.append(Line2D([], [], color=color, linewidth=width, linestyle=dash,
                              marker=marker, markersize=size,
                              markeredgecolor=edge, markeredgewidth=0.5,
                              label=name if paper
                              else f"{name} — {trace.overall:.2f}%"))

    # Overlays sit on top of the captures: they are the result being added to
    # an existing figure, so they must not be hidden under it. Slots continue
    # past the captures' so an overlay never steals a capture's shape.
    for n, (name, series_by_phy) in enumerate(OVERLAYS):
        for phy, pts in series_by_phy:
            marker, dash = style(len(slots) + n, paper)
            color = PHY_COLOUR.get(phy, PHY_COLOUR[None])
            every = max(1, round(len(pts) / 12)) if paper else 1
            ax.plot([p[0] for p in pts], [p[1] for p in pts],
                    color=color, linewidth=width, linestyle=dash, marker=marker,
                    markersize=size, markeredgecolor=edge, markeredgewidth=0.5,
                    markevery=every, zorder=3)
            label = f"{name} {phy}" if phy and len(series_by_phy) > 1 else name
            handles.append(Line2D([], [], color=color, linewidth=width,
                                  linestyle=dash, marker=marker, markersize=size,
                                  markeredgecolor=edge, markeredgewidth=0.5,
                                  label=label))

    ax.set_ylim(-2, 102)
    if logx:
        # The interesting part of a sweep is where PER breaks down, which sits
        # at the fast end; on a linear axis a 2-200 ms sweep crushes it into
        # the first few percent of the width. Ticks are set explicitly and
        # formatted as plain numbers — a reader wants "5 ms", not "10^0.7".
        ax.set_xscale("log")
        ticks = [t for t in (1, 2, 3, 5, 10, 20, 30, 50, 100, 150, 200)
                 if not xlim or xlim[0] <= t <= xlim[1]]
        ax.set_xticks(ticks)
        ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
        ax.xaxis.set_minor_formatter(NullFormatter())
    if xlim:
        ax.set_xlim(*xlim)

    if paper:
        ax.legend(handles=handles, loc=legend_loc or "upper right", frameon=True,
                  framealpha=1, edgecolor="#b0b0b0", fancybox=False,
                  borderpad=0.5, handlelength=2.8, labelspacing=0.35)
        ax.get_legend().get_frame().set_linewidth(0.5)
    elif legend_loc:
        # Asked for inside the axes: one column, since a wide multi-column
        # block placed over the data hides more than it explains.
        ax.legend(handles=handles, frameon=False, labelcolor=MUTED,
                  loc=legend_loc, fontsize=9, handlelength=2.6)
        if ax_legend is not None:
            ax_legend.set_visible(False)
    else:
        # The legend gets its own strip under the panel: overlaid on the axes it
        # covers the curves as soon as more than a few captures are ticked.
        ax_legend.legend(handles=handles, frameon=False, labelcolor=MUTED,
                         loc="upper left", fontsize=9, ncols=min(len(handles), 3),
                         borderaxespad=0, handlelength=2.6, columnspacing=1.6)


# ---------------------------------------------------------------- ui


def controls(fig, axes, traces, slots, active, window, title, xlim=None,
             labels=None, markers=None):
    """Wire the checkboxes, the window box and the all/none buttons."""
    state = {"window": window, "quiet": False}

    def redraw():
        if state["quiet"]:
            return
        draw(axes, traces, slots, active, state["window"], title, xlim, labels,
             markers=markers)
        fig.canvas.draw_idle()

    colors = [PHY_COLOUR[t.phy] for t in traces]
    ax_check = fig.add_axes([0.012, 0.22, 0.22, 0.71])
    ax_check.set_facecolor(SURFACE)
    ax_check.set_title("captures", color=INK, loc="left", fontsize=11)
    for spine in ax_check.spines.values():
        spine.set_visible(False)
    checks = CheckButtons(
        ax_check, [t.label for t in traces], active,
        label_props={"color": [INK] * len(traces), "fontsize": [8] * len(traces)},
        frame_props={"edgecolor": colors, "facecolor": "none", "s": 60},
        check_props={"facecolor": colors, "s": 60},
    )

    # `get_status()` is the state after the click, so mirror it rather than
    # trying to reconstruct which box moved.
    def on_toggle(_label):
        active[:] = list(checks.get_status())
        redraw()

    checks.on_clicked(on_toggle)

    ax_box = fig.add_axes([0.105, 0.12, 0.075, 0.045])
    box = TextBox(ax_box, "max packets  ", initial=str(window),
                  color=SURFACE, hovercolor="#f1f0ea")
    box.label.set_color(MUTED)
    box.label.set_fontsize(9)

    def on_submit(text):
        try:
            value = int(text)
        except ValueError:
            value = 0
        if value < 2:
            box.set_val(str(state["window"]))
            return
        state["window"] = value
        summarise([t for i, t in enumerate(traces) if active[i]], value)
        redraw()

    box.on_submit(on_submit)

    ax_all = fig.add_axes([0.015, 0.05, 0.085, 0.045])
    ax_none = fig.add_axes([0.115, 0.05, 0.085, 0.045])
    all_btn = Button(ax_all, "all", color=SURFACE, hovercolor="#f1f0ea")
    none_btn = Button(ax_none, "none", color=SURFACE, hovercolor="#f1f0ea")
    for btn in (all_btn, none_btn):
        btn.label.set_color(MUTED)
        btn.label.set_fontsize(9)

    def set_all(value):
        # `set_active` fires the toggle callback, so hold the redraw until the
        # last box has moved rather than repainting once per capture.
        state["quiet"] = True
        for i in range(len(traces)):
            if checks.get_status()[i] != value:
                checks.set_active(i)
        state["quiet"] = False
        redraw()

    all_btn.on_clicked(lambda _e: set_all(True))
    none_btn.on_clicked(lambda _e: set_all(False))

    # Widgets are garbage collected the moment nothing references them.
    return checks, box, all_btn, none_btn


def main():
    here = Path(__file__).resolve().parent
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--csv-dir", type=Path, default=here.parent / "csv",
                   help="directory of captures to offer (default ../csv)")
    p.add_argument("--window", type=int, default=1000,
                   help="transmitted packets per PER point (default 1000)")
    p.add_argument("--preset", choices=sorted(PRESETS),
                   help="tick a named comparison instead of the first capture")
    p.add_argument("--select", nargs="*", default=None,
                   help="captures to tick at startup, by file stem")
    p.add_argument("--title", default="packet error rate vs transmit IFS")
    p.add_argument("--run-size", action="append", default=[], metavar="[STEM=]N",
                   help="packets the transmitter sends at each IFS step, "
                        "overriding the period inferred from the step stamps. "
                        "Bare N applies to every capture; STEM=N to one, "
                        "repeatable, for figures mixing different schedules")
    p.add_argument("--ifs-start", type=float, default=SCHEDULE["ifs_start"],
                   help="IFS of the first run, ms (default 10)")
    p.add_argument("--ifs-step", type=float, default=SCHEDULE["ifs_step"],
                   help="IFS change per run, ms (default -0.1)")
    p.add_argument("--no-windows", action="store_true",
                   help="draw only the trend line, without the faint mark per "
                        "measurement window behind it")
    p.add_argument("--legend-loc", metavar="LOC",
                   help="draw the legend inside the axes at a matplotlib "
                        "location, e.g. 'upper left'; default is a strip under "
                        "the panel, or upper right with --paper")
    p.add_argument("--logx", action="store_true",
                   help="log the IFS axis — the breakdown region is at the "
                        "fast end and a linear axis crushes it")
    p.add_argument("--xlim", nargs=2, type=float, metavar=("MIN", "MAX"),
                   help="clip the IFS axis to this range, in ms")
    p.add_argument("--merge-phy", action="store_true",
                   help="report a capture carrying two PHYs as one pooled "
                        "trace instead of splitting it, for when both halves "
                        "see the same rate")
    p.add_argument("--paper", action="store_true",
                   help="journal styling: serif type, boxed frame, inward "
                        "ticks, no title, legend inside the axes")
    p.add_argument("--figsize", nargs=2, type=float, metavar=("W", "H"),
                   help="figure size in inches (default 13x7.6, or 3.5x2.6 "
                        "with --paper — one journal column, which is what "
                        "makes the type and marks large against the axes)")
    p.add_argument("--label", action="append", default=[], metavar="STEM=NAME",
                   help="legend name for a capture, e.g. "
                        "halow_switchv2mcs0='802.11ah, MCS0'; repeatable")
    p.add_argument("--marker", action="append", default=[], metavar="STEM=SHAPE",
                   help=f"matplotlib marker for a capture, e.g. "
                        f"zigbee_swap2ZIG=s; repeatable. Default order: "
                        f"{' '.join(MARKERS)}")
    p.add_argument("--overlay", action="append", default=[], metavar="[NAME=]PATH",
                   help="draw a pre-aggregated PER curve from a CSV alongside "
                        "the captures, e.g. "
                        "--overlay 'replay=../recording/bench/results/per_replay.csv'. "
                        "Repeatable. Accepts `ifs_ms,per_H_pct,per_Z_pct` or "
                        "`ifs_ms,phy,per_pct`.")
    p.add_argument("--overlay-pool", action="store_true",
                   help="collapse each overlay's PHYs into one curve, pooling "
                        "the underlying counts where the file has them")
    p.add_argument("--overlay-split", action="store_true",
                   help="keep an overlay's PHY curves separate even when they "
                        "are identical (by default they are pooled into one)")
    p.add_argument("--out", type=Path,
                   help="save instead of opening a window; no controls are drawn")
    args = p.parse_args()

    for spec in args.run_size:
        stem, _, value = spec.rpartition("=")
        try:
            n = max(int(value), 1)
        except ValueError:
            p.error(f"--run-size {spec!r}: expected N or STEM=N")
        if stem:
            RUN_SIZE_BY_STEM[stem] = n
        else:
            globals()["RUN_SIZE_OVERRIDE"] = n
    globals()["MERGE_PHY"] = args.merge_phy
    SCHEDULE.update(run_size=max(RUN_SIZE_OVERRIDE or SCHEDULE["run_size"], 1),
                    ifs_start=args.ifs_start, ifs_step=args.ifs_step)

    if not args.csv_dir.is_dir():
        sys.exit(f"missing {args.csv_dir} — put the captures there first")
    if args.window < 2:
        sys.exit("--window must be at least 2 packets")

    traces, skipped = load_dir(args.csv_dir)
    if not traces:
        sys.exit(f"{args.csv_dir}: no capture had a usable packet index")

    # Shape is per file, not per trace, so a capture's two PHYs share a marker
    # and differ only in the hue that names their PHY.
    #
    # Slot order follows the requested captures, not the directory listing.
    # Indexing by alphabetical position among every file in `csv/` makes the
    # marker and dash an accident of what else happens to sit there, so the
    # same capture changes appearance between figures and two selected
    # captures can collide. Ordering by the selection instead means the first
    # capture asked for is always solid, the second dashed, the third dotted.
    stems = sorted({t.file for t in traces})
    chosen = PRESETS[args.preset] if args.preset else (args.select or [])
    order = [c for c in chosen if c in stems]
    slots = {stem: i for i, stem in enumerate(order + [s for s in stems if s not in order])}
    if len(slots) > MAX_FILES:
        sys.exit(f"{len(slots)} captures but only {MAX_FILES} shape/dash pairs — "
                 f"narrow {args.csv_dir} down")

    for name, reason in skipped:
        print(f"skipped {name}: {reason}")

    wanted = chosen if chosen else None
    if wanted is None:
        active = [i == 0 for i in range(len(traces))]
    else:
        active = [t.file in wanted for t in traces]
        missing = [w for w in wanted if not any(t.file == w for t in traces)]
        if missing:
            print(f"not plotted, see above: {', '.join(missing)}")
        if not any(active) and not args.overlay:
            sys.exit("none of the requested captures had a usable packet index")

    labels, markers = {}, {}
    for flag, pairs, into in (("--label", args.label, labels),
                              ("--marker", args.marker, markers)):
        for pair in pairs:
            stem, _, value = pair.partition("=")
            if not value:
                sys.exit(f"{flag} wants STEM=VALUE, got {pair!r}")
            into[stem] = value

    global OVERLAYS, OVERLAY_SPLIT, OVERLAY_POOL
    OVERLAY_SPLIT = args.overlay_split
    OVERLAY_POOL = args.overlay_pool
    OVERLAYS = [load_overlay(spec) for spec in args.overlay]
    for name, series_by_phy in OVERLAYS:
        for phy, pts in series_by_phy:
            print(f"overlay {name} {phy or '(both PHYs pooled)'}: {len(pts)} point(s), "
                  f"IFS {min(p[0] for p in pts):.2f}-{max(p[0] for p in pts):.2f} ms, "
                  f"PER {min(p[1] for p in pts):.2f}-{max(p[1] for p in pts):.2f}%")

    selected = [t for i, t in enumerate(traces) if active[i]]
    summarise(selected, args.window)

    if args.paper:
        if not args.out:
            sys.exit("--paper is for a saved figure; give --out too")
        plt.rcParams.update(PAPER_RC)
        fig, ax = plt.subplots(figsize=tuple(args.figsize or (3.5, 2.6)),
                               layout="constrained")
        draw((ax, None), traces, slots, active, args.window, args.title,
             args.xlim, labels, paper=True, markers=markers, logx=args.logx,
             legend_loc=args.legend_loc, show_windows=not args.no_windows)
        fig.savefig(args.out, bbox_inches="tight", pad_inches=0.02,
                    dpi=600, transparent=True)
        print(f"\nwrote {args.out}")
        print(f"suggested caption: {args.title}. Packet error rate is measured "
              f"over consecutive windows of {args.window} transmitted packets; "
              f"faint marks are the individual windows and the line their "
              f"pooled rate at each inter-frame spacing.")
        return

    fig = plt.figure(figsize=tuple(args.figsize or (13, 7.6)))
    fig.patch.set_facecolor(SURFACE)
    left = 0.06 if args.out else 0.27
    width = 0.985 - left
    axes = (fig.add_axes([left, 0.28, width, 0.66]),   # PER vs IFS
            fig.add_axes([left, 0.01, width, 0.16]))   # legend strip

    if args.out:
        draw(axes, traces, slots, active, args.window, args.title, args.xlim,
             labels, markers=markers, logx=args.logx, legend_loc=args.legend_loc,
             show_windows=not args.no_windows)
        fig.savefig(args.out, dpi=150)
        print(f"\nwrote {args.out}")
        return

    _widgets = controls(fig, axes, traces, slots, active, args.window,
                        args.title, args.xlim, labels, markers)
    draw(axes, traces, slots, active, args.window, args.title, args.xlim,
         labels, markers=markers, logx=args.logx, legend_loc=args.legend_loc,
         show_windows=not args.no_windows)
    plt.show()


if __name__ == "__main__":
    main()
