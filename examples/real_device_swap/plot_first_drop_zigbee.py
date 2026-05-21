#!/usr/bin/env python3
"""100%-stacked histogram of zigbee_swap reception, per wait bin.

Counterpart of `plot_first_drop.py` for the multizig firmware: the TX side
alternates between two Zigbee channels (A: 2.425 GHz, B: 2.45 GHz). The
on-wire wait field is u32 microseconds (full µs range, no cap); for
display we convert to milliseconds. The `run` counter in `zigbee_swap.csv`
makes per-run classification exact, including runs with zero receptions.

The sweep range is auto-detected from the union of waits observed across
all runs. Each bin is one distinct wait_us value; the x-axis labels show
ms (wait_us / 1000) for readability.

Outputs:
    first_drop_zigbee.png             — aggregate stacked histogram
    first_drop_runs_zigbee.png        — per-run heatmap
    first_drop_phy_share_zigbee.png   — channel A vs B receive mix per run
"""
import csv
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import ListedColormap

CSV_PATH = Path(__file__).with_name("zigbee_swap.csv")
OUT_PATH = Path(__file__).with_name("first_drop_zigbee.png")
RUNS_OUT_PATH = Path(__file__).with_name("first_drop_runs_zigbee.png")
PHY_SHARE_OUT_PATH = Path(__file__).with_name("first_drop_phy_share_zigbee.png")

STREAK_BUCKETS = [
    ("rcv_1",       1,    1,    "1"),
    ("rcv_2",       2,    2,    "2"),
    ("rcv_3_4",     3,    4,    "3-4"),
    ("rcv_5_8",     5,    8,    "5-8"),
    ("rcv_9_15",    9,   15,    "9-15"),
    ("rcv_16_31",  16,   31,    "16-31"),
    ("rcv_32_63",  32,   63,    "32-63"),
    ("rcv_64_127", 64,  127,    "64-127"),
    ("rcv_128_255",128, 255,    "128-255"),
    ("rcv_256p",  256, 10_000,  "≥256"),
]

STACK_ORDER = ["drop_first", "drop_other"] + [b[0] for b in STREAK_BUCKETS]

_cmap = plt.get_cmap("RdYlGn")
_n_colors = len(STACK_ORDER)
COLORS = {c: _cmap(i / (_n_colors - 1)) for i, c in enumerate(STACK_ORDER)}
COLORS["drop_first"] = "#ffffff"
COLORS["drop_other"] = "#000000"

LABELS = {
    "drop_first": "first drop of sweep",
    "drop_other": "subsequent drop",
}
for key, _lo, _hi, label in STREAK_BUCKETS:
    LABELS[key] = f"rcv streak {label}"

CATEGORY_INDEX = {key: idx for idx, key in enumerate(STACK_ORDER)}
PHY_COLORS = {
    "A": "#1f77b4",
    "B": "#ff7f0e",
}
PHY_LABELS = {
    "A": "zigbee A (2.425 GHz)",
    "B": "zigbee B (2.45 GHz)",
}


def load_rows(path: Path):
    """Return (rows, phy_counts_by_run).

    rows: list of (run, step, wait_us) for every successfully-parsed rx frame.
    phy_counts_by_run: {run: {"A": n, "B": n}} from the `phy_active` column.
    """
    rows = []
    phy_counts_by_run: dict[int, dict[str, int]] = {}
    with path.open(newline="") as f:
        reader = csv.DictReader(f)
        for row in reader:
            if row.get("frame_event") != "rx":
                continue
            try:
                run = int(row["run"])
                step = int(row["step"])
                wait = int(row["wait_us"])
            except (KeyError, TypeError, ValueError):
                continue
            if run < 0 or step < 0 or wait < 0:
                continue
            rows.append((run, step, wait))
            phy = row.get("phy_active", "")
            if phy in PHY_LABELS:
                per_run = phy_counts_by_run.setdefault(run, {"A": 0, "B": 0})
                per_run[phy] += 1
    return rows, phy_counts_by_run


def streak_bucket_key(run_len: int) -> str:
    for key, lo, hi, _ in STREAK_BUCKETS:
        if lo <= run_len <= hi:
            return key
    return STREAK_BUCKETS[-1][0]


def run_length_in_set(value: int, sorted_index: dict[int, int], ordered: list[int]) -> int:
    """Length of the contiguous (by sweep position) streak containing `value`.

    `value` is a wait_us reading; `sorted_index` maps each expected wait_us to
    its position in the sweep; `ordered` is the sweep itself. A streak is a
    maximal run of *positions* whose wait_us are all in the received set.
    """
    if value not in sorted_index:
        return 0
    return 1  # placeholder, real computation done in classify_bucket


def classify_bucket(bucket: dict, ordered_waits: list[int], n_bins: int) -> np.ndarray:
    """Classify each sweep position as drop_first / drop_other / rcv_<bucket>.

    `ordered_waits[i]` is the i-th wait_us in the sweep (here we use ascending
    order; the visual axis is wait_us, which is monotonic).
    """
    categories = np.full(n_bins, CATEGORY_INDEX["drop_other"], dtype=int)
    recv_set = bucket["received_ms"]
    miss_set = bucket["missed_ms_set"]
    first_miss = bucket["first_missed_ms"]

    received_mask = np.array([w in recv_set for w in ordered_waits], dtype=bool)

    # Compute streak length around each received position (contiguous in sweep order).
    i = 0
    while i < n_bins:
        if not received_mask[i]:
            i += 1
            continue
        j = i
        while j < n_bins and received_mask[j]:
            j += 1
        length = j - i
        bucket_key = streak_bucket_key(length)
        for k in range(i, j):
            categories[k] = CATEGORY_INDEX[bucket_key]
        i = j

    if first_miss is not None and first_miss in miss_set:
        # Map wait_us → position in ordered_waits.
        try:
            pos = ordered_waits.index(first_miss)
            categories[pos] = CATEGORY_INDEX["drop_first"]
        except ValueError:
            pass

    return categories


def plot_aggregate_view(fracs, counts, total_runs, ordered_waits):
    n_bins = len(ordered_waits)
    x = np.arange(n_bins)
    bottom = np.zeros(n_bins)
    fig, ax = plt.subplots(figsize=(14, 6))
    ax.set_facecolor("#e8e8e8")
    for category in STACK_ORDER:
        ax.bar(
            x,
            fracs[category],
            bottom=bottom,
            color=COLORS[category],
            width=1.0,
            label=f"{LABELS[category]} ({int(counts[category].sum())})",
        )
        bottom += fracs[category]

    ax.set_xlim(-0.5, n_bins - 0.5)
    ax.set_ylim(0, 1.0)
    _apply_wait_ms_ticks(ax, ordered_waits)
    _wire_cursor(ax, ordered_waits)
    ax.set_xlabel("wait (TX inter-frame delay, ms)")
    ax.set_ylabel("fraction of runs")
    ax.set_title(f"zigbee_swap reception by streak length — {total_runs} run(s)")
    ax.legend(loc="upper center", bbox_to_anchor=(0.5, -0.12), ncol=4, fontsize=8)
    ax.grid(True, axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(OUT_PATH, dpi=130, bbox_inches="tight")
    return fig


def plot_runs_view(classified_runs: np.ndarray, total_runs: int, ordered_waits, run_ids):
    n_bins = len(ordered_waits)
    fig_height = max(4.5, min(0.35 * total_runs + 2.0, 18.0))
    fig, ax = plt.subplots(figsize=(14, fig_height))
    cmap = ListedColormap([COLORS[key] for key in STACK_ORDER])
    ax.imshow(
        classified_runs,
        aspect="auto",
        interpolation="nearest",
        cmap=cmap,
        vmin=-0.5,
        vmax=len(STACK_ORDER) - 0.5,
        origin="upper",
    )
    ax.set_xlim(-0.5, n_bins - 0.5)
    _apply_wait_ms_ticks(ax, ordered_waits)
    _wire_cursor(ax, ordered_waits, run_ids=run_ids)
    ax.set_xlabel("wait (TX inter-frame delay, ms)")
    ax.set_ylabel("run index")
    ax.set_title("zigbee_swap reception by run — unmerged stacked rows")

    tick_count = min(total_runs, 16)
    if tick_count > 0:
        tick_positions = np.linspace(0, total_runs - 1, num=tick_count, dtype=int)
        ax.set_yticks(tick_positions)
        ax.set_yticklabels([str(value) for value in tick_positions])

    handles = [
        plt.Rectangle((0, 0), 1, 1, facecolor=COLORS[key], edgecolor="none")
        for key in STACK_ORDER
    ]
    ax.legend(
        handles,
        [LABELS[key] for key in STACK_ORDER],
        loc="upper center",
        bbox_to_anchor=(0.5, -0.12),
        ncol=4,
        fontsize=8,
    )
    fig.tight_layout()
    fig.savefig(RUNS_OUT_PATH, dpi=130, bbox_inches="tight")
    return fig


def plot_phy_share_view(phy_share_rows: np.ndarray, run_ids: list[int]):
    fig, ax = plt.subplots(figsize=(14, 5.5))
    x = np.arange(len(run_ids))
    bottom = np.zeros(len(run_ids))

    for phy in ("A", "B"):
        values = phy_share_rows[:, 0] if phy == "A" else phy_share_rows[:, 1]
        ax.bar(
            x,
            values,
            bottom=bottom,
            color=PHY_COLORS[phy],
            width=0.9,
            label=PHY_LABELS[phy],
        )
        bottom += values

    tick_count = min(len(run_ids), 16)
    if tick_count > 0:
        tick_positions = np.linspace(0, len(run_ids) - 1, num=tick_count, dtype=int)
        ax.set_xticks(tick_positions)
        ax.set_xticklabels([str(run_ids[idx]) for idx in tick_positions])

    ax.set_xlim(-0.5, len(run_ids) - 0.5)
    ax.set_ylim(0.0, 1.0)

    def fmt(x, y):
        idx = int(round(x))
        if 0 <= idx < len(run_ids):
            return f"run={run_ids[idx]}, share={y:.3f}"
        return f"x={x:.2f}, y={y:.3f}"
    ax.format_coord = fmt

    ax.set_xlabel("run")
    ax.set_ylabel("fraction of received packets")
    ax.set_title("zigbee_swap receive mix by run — channel A vs B")
    ax.legend(loc="upper center", bbox_to_anchor=(0.5, -0.12), ncol=2)
    ax.grid(True, axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(PHY_SHARE_OUT_PATH, dpi=130, bbox_inches="tight")
    return fig


def _apply_wait_ms_ticks(ax, ordered_waits):
    """Label up to 12 ticks with wait_us / 1000 (ms) at those positions."""
    n = len(ordered_waits)
    if n == 0:
        return
    tick_count = min(n, 12)
    tick_positions = np.linspace(0, n - 1, num=tick_count, dtype=int)
    ax.set_xticks(tick_positions)
    ax.set_xticklabels([f"{ordered_waits[i] / 1000.0:.2f}" for i in tick_positions])


def _wire_cursor(ax, ordered_waits, run_ids=None):
    """Override format_coord so the toolbar shows the real wait_ms (and run id)
    at the cursor position instead of the raw bin index / row number."""
    n = len(ordered_waits)
    rids = run_ids if run_ids is not None else None

    def fmt(x, y):
        idx = int(round(x))
        if 0 <= idx < n:
            wait_ms = ordered_waits[idx] / 1000.0
            x_str = f"wait={wait_ms:.3f} ms (bin {idx})"
        else:
            x_str = f"x={x:.2f}"
        if rids is not None:
            row = int(round(y))
            if 0 <= row < len(rids):
                return f"{x_str}, run={rids[row]}"
            return f"{x_str}, y={y:.2f}"
        return f"{x_str}, y={y:.3f}"

    ax.format_coord = fmt


def build_runs(rows: list[tuple[int, int, int]]):
    received_by_run: dict[int, set[int]] = {}
    all_waits: set[int] = set()

    for run, _step, wait in rows:
        received_by_run.setdefault(run, set()).add(wait)
        all_waits.add(wait)

    if not received_by_run:
        return [], [], 0

    ordered_waits = sorted(all_waits)
    expected = set(ordered_waits)

    run_start = min(received_by_run)
    run_stop = max(received_by_run)
    runs = []
    for run in range(run_start, run_stop + 1):
        received_ms = received_by_run.get(run, set())
        missed_ms_set = expected - received_ms
        # "First drop" in microsecond sweep direction: the largest wait_us
        # that was missed (since the TX sweeps from large → small wait, the
        # first missed bin in sweep order is the largest missed wait_us).
        first_missed_ms = max(missed_ms_set) if missed_ms_set else None
        runs.append(
            {
                "run": run,
                "received_ms": received_ms,
                "missed_ms_set": missed_ms_set,
                "first_missed_ms": first_missed_ms,
            }
        )
    return runs, ordered_waits, len(received_by_run)


def build_phy_share_rows(phy_counts_by_run: dict[int, dict[str, int]], run_ids: list[int]) -> np.ndarray:
    rows = []
    for run_id in run_ids:
        counts = phy_counts_by_run.get(run_id, {"A": 0, "B": 0})
        total = counts["A"] + counts["B"]
        if total == 0:
            rows.append((0.0, 0.0))
        else:
            rows.append((counts["A"] / total, counts["B"] / total))
    return np.asarray(rows, dtype=float)


def main():
    if not CSV_PATH.exists():
        sys.exit(f"missing {CSV_PATH} — run `zigbee_swap` first")

    rows, phy_counts_by_run = load_rows(CSV_PATH)
    if not rows:
        sys.exit(f"{CSV_PATH} has no decoded frames")

    runs, ordered_waits, runs_with_hits = build_runs(rows)
    total_runs = len(runs)
    if total_runs == 0 or not ordered_waits:
        sys.exit(f"{CSV_PATH} has no valid runs")

    print(
        f"using explicit run counters from {CSV_PATH.name}: "
        f"{runs_with_hits} run(s) with receptions, {total_runs} total run(s) in range"
    )
    print(
        f"wait bins: {len(ordered_waits)} unique values from "
        f"{ordered_waits[0] / 1000.0:.3f} ms to {ordered_waits[-1] / 1000.0:.3f} ms"
    )

    n_bins = len(ordered_waits)
    counts = {c: np.zeros(n_bins, dtype=int) for c in STACK_ORDER}
    classified_runs = []

    for bucket in runs:
        bucket_classes = classify_bucket(bucket, ordered_waits, n_bins)
        classified_runs.append(bucket_classes)
        for category in STACK_ORDER:
            counts[category] += (bucket_classes == CATEGORY_INDEX[category]).astype(int)

    fracs = {c: counts[c] / total_runs for c in STACK_ORDER}
    classified_runs = np.vstack(classified_runs)
    run_ids = [bucket["run"] for bucket in runs]

    plot_aggregate_view(fracs, counts, total_runs, ordered_waits)
    plot_runs_view(classified_runs, total_runs, ordered_waits, run_ids)
    phy_share_rows = build_phy_share_rows(phy_counts_by_run, run_ids)
    plot_phy_share_view(phy_share_rows, run_ids)
    plt.show()
    print(f"wrote {OUT_PATH}")
    print(f"wrote {RUNS_OUT_PATH}")
    print(f"wrote {PHY_SHARE_OUT_PATH}")
    print(f"runs: {total_runs}")
    for c in STACK_ORDER:
        print(f"  {LABELS[c]:25s} total events: {int(counts[c].sum())}")


if __name__ == "__main__":
    main()
