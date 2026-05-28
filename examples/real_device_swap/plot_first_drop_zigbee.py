#!/usr/bin/env python3
"""100%-stacked histogram of zigbee_swap reception, per wait_ms bin.

Counterpart of `plot_first_drop.py` for the multizig firmware: the TX side
alternates between two Zigbee channels (A: 2.425 GHz, B: 2.45 GHz). Each
frame carries a `step` counter and a `wait_us` field giving the inter-frame
delay TX waited after that frame.

X-axis = wait_ms ascending (0 ms left → max ms right), matching the
original `plot_first_drop.py` orientation. The TX sweep schedule is
auto-detected:
  - quantum  = min |Δwait| between consecutive received steps
  - schedule = [0, quantum, 2·quantum, ..., max(observed_wait)]

Drops = schedule − received per run. So waits that were never received in
any run still show as drops, and 0 ms is always a bin even if the TX
firmware doesn't sweep that low (then it just shows as a perpetual drop).

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
PHY_COLORS = {"A": "#1f77b4", "B": "#ff7f0e"}
PHY_LABELS = {
    "A": "zigbee A (2.425 GHz)",
    "B": "zigbee B (2.45 GHz)",
}


def load_rows(path: Path):
    """Return (rows, phy_counts_by_run)."""
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


def infer_schedule(rows):
    """Infer the TX sweep schedule and direction.

    Returns (schedule_us, quantum_us, sweep_direction).
        schedule_us: sorted list of expected wait_us values from 0 to max.
        quantum_us:  smallest |Δwait| between adjacent steps observed.
        sweep_direction: "high_to_low" if step 0 has higher wait than step N,
                         else "low_to_high". Used to pick "first drop".
    """
    step_to_wait: dict[int, int] = {}
    for _run, step, wait in rows:
        # last-write-wins; sweep schedule is assumed stable across runs
        step_to_wait[step] = wait

    if not step_to_wait:
        return [], 1000, "low_to_high"

    by_step = sorted(step_to_wait.items())

    diffs = []
    for (s1, w1), (s2, w2) in zip(by_step, by_step[1:]):
        if s2 == s1 + 1:
            d = abs(w2 - w1)
            if d > 0:
                diffs.append(d)
    quantum_us = min(diffs) if diffs else 1000

    max_wait = max(step_to_wait.values())
    schedule = list(range(0, max_wait + 1, quantum_us))
    if schedule[-1] != max_wait:
        schedule.append(max_wait)

    direction = "high_to_low" if by_step[0][1] > by_step[-1][1] else "low_to_high"
    return schedule, quantum_us, direction


def streak_bucket_key(run_len: int) -> str:
    for key, lo, hi, _ in STREAK_BUCKETS:
        if lo <= run_len <= hi:
            return key
    return STREAK_BUCKETS[-1][0]


def build_runs(rows, schedule_us, sweep_direction):
    """Per-run reception analysis against the inferred schedule."""
    per_run_received: dict[int, set[int]] = {}
    for run, _step, wait in rows:
        per_run_received.setdefault(run, set()).add(wait)

    schedule_set = set(schedule_us)
    runs = []
    for run_id in sorted(per_run_received.keys()):
        received = per_run_received[run_id] & schedule_set
        missed = schedule_set - received
        if missed:
            first_missed = max(missed) if sweep_direction == "high_to_low" else min(missed)
        else:
            first_missed = None
        runs.append({
            "run": run_id,
            "received_us": received,
            "missed_us": missed,
            "first_missed": first_missed,
        })
    return runs


def classify_bucket(bucket: dict, schedule_us: list[int]) -> np.ndarray:
    """Classify each schedule bin as drop_first / drop_other / rcv_<bucket>."""
    n = len(schedule_us)
    categories = np.full(n, CATEGORY_INDEX["drop_other"], dtype=int)
    received = bucket["received_us"]

    # Streaks are contiguous runs in schedule order; matches TX time order
    # (up to direction reversal — streak length is invariant either way).
    received_mask = np.array([w in received for w in schedule_us], dtype=bool)
    i = 0
    while i < n:
        if not received_mask[i]:
            i += 1
            continue
        j = i
        while j < n and received_mask[j]:
            j += 1
        length = j - i
        bucket_key = streak_bucket_key(length)
        for k in range(i, j):
            categories[k] = CATEGORY_INDEX[bucket_key]
        i = j

    first_missed = bucket["first_missed"]
    if first_missed is not None:
        try:
            pos = schedule_us.index(first_missed)
            categories[pos] = CATEGORY_INDEX["drop_first"]
        except ValueError:
            pass
    return categories


def _apply_wait_ms_ticks(ax, schedule_us):
    """Label up to 12 ticks with wait_ms = wait_us / 1000."""
    n = len(schedule_us)
    if n == 0:
        return
    tick_count = min(n, 12)
    tick_positions = np.linspace(0, n - 1, num=tick_count, dtype=int)
    ax.set_xticks(tick_positions)
    ax.set_xticklabels([f"{schedule_us[i] / 1000.0:.1f}" for i in tick_positions])


def _wire_cursor(ax, schedule_us, run_ids=None):
    n = len(schedule_us)

    def fmt(x, y):
        idx = int(round(x))
        if 0 <= idx < n:
            x_str = f"wait={schedule_us[idx] / 1000.0:.3f} ms (bin {idx})"
        else:
            x_str = f"x={x:.2f}"
        if run_ids is not None:
            row = int(round(y))
            if 0 <= row < len(run_ids):
                return f"{x_str}, run={run_ids[row]}"
            return f"{x_str}, y={y:.2f}"
        return f"{x_str}, frac={y:.3f}"
    ax.format_coord = fmt


def plot_aggregate_view(fracs, counts, total_runs, schedule_us):
    n = len(schedule_us)
    x = np.arange(n)
    bottom = np.zeros(n)
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

    ax.set_xlim(-0.5, n - 0.5)
    ax.set_ylim(0, 1.0)
    _apply_wait_ms_ticks(ax, schedule_us)
    _wire_cursor(ax, schedule_us)
    ax.set_xlabel("wait (TX inter-frame delay, ms) — 0 ms left → max right")
    ax.set_ylabel("fraction of runs")
    ax.set_title(f"zigbee_swap reception by streak length — {total_runs} run(s)")
    ax.legend(loc="upper center", bbox_to_anchor=(0.5, -0.12), ncol=4, fontsize=8)
    ax.grid(True, axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(OUT_PATH, dpi=130, bbox_inches="tight")
    return fig


def plot_runs_view(classified_runs, total_runs, schedule_us, run_ids):
    n = len(schedule_us)
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
    ax.set_xlim(-0.5, n - 0.5)
    _apply_wait_ms_ticks(ax, schedule_us)
    _wire_cursor(ax, schedule_us, run_ids=run_ids)
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


def plot_phy_share_view(phy_share_rows, run_ids):
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


def build_phy_share_rows(phy_counts_by_run, run_ids):
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

    schedule_us, quantum_us, sweep_direction = infer_schedule(rows)
    if not schedule_us:
        sys.exit(f"{CSV_PATH} has no usable schedule")

    runs = build_runs(rows, schedule_us, sweep_direction)
    total_runs = len(runs)
    if total_runs == 0:
        sys.exit(f"{CSV_PATH} has no valid runs")

    print(
        f"runs: {total_runs}, schedule: {len(schedule_us)} bins from "
        f"0 ms to {schedule_us[-1] / 1000.0:.3f} ms "
        f"(quantum {quantum_us / 1000.0:.3f} ms, sweep {sweep_direction})"
    )

    counts = {c: np.zeros(len(schedule_us), dtype=int) for c in STACK_ORDER}
    classified_runs = []
    for bucket in runs:
        bucket_classes = classify_bucket(bucket, schedule_us)
        classified_runs.append(bucket_classes)
        for category in STACK_ORDER:
            counts[category] += (bucket_classes == CATEGORY_INDEX[category]).astype(int)

    fracs = {c: counts[c] / total_runs for c in STACK_ORDER}
    classified_runs = np.vstack(classified_runs)
    run_ids = [bucket["run"] for bucket in runs]

    plot_aggregate_view(fracs, counts, total_runs, schedule_us)
    plot_runs_view(classified_runs, total_runs, schedule_us, run_ids)
    phy_share_rows = build_phy_share_rows(phy_counts_by_run, run_ids)
    plot_phy_share_view(phy_share_rows, run_ids)
    plt.show()
    print(f"wrote {OUT_PATH}")
    print(f"wrote {RUNS_OUT_PATH}")
    print(f"wrote {PHY_SHARE_OUT_PATH}")
    for c in STACK_ORDER:
        print(f"  {LABELS[c]:25s} total events: {int(counts[c].sum())}")


if __name__ == "__main__":
    main()
