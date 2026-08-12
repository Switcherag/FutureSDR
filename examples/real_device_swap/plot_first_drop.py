#!/usr/bin/env python3
"""100%-stacked histogram of per_frame_swap reception, per wait_ms bin.

Every rate on these plots is a percentage, never a 0-1 fraction.

Reads `per_frame_swap.csv`, which carries the explicit `run` counter from the
swap controller output. That lets the script classify each `wait_ms` bin
exactly once for every run in the observed range, including runs with zero
receptions. Older captures can still fall back to `data/network.log`, using
the previous step-gap heuristic.

Each ms in a sweep is tagged with one of these categories, ordered from
"worst" → "best" (deep red → green) using the diverging RdYlGn scale
(the standard "error" colormap):

    drop_first        — first dropped frame in the sweep
    drop_other        — every subsequent dropped frame
    rcv streak 1      — isolated received frame
    rcv streak 2
    rcv streak 3-4
    rcv streak 5-8
    rcv streak 9-15
    rcv streak 16-31
    rcv streak 32-63
    rcv streak 64-127
    rcv streak 128-255
    rcv streak ≥256

A frame that was the first decode after an interrupt is *still* counted
inside the streak it belongs to — the streak is the maximal connected
run of received ms's around that frame.

Every sweep contributes exactly one event per ms, so each ms bar reaches
1.0 (full unit rectangle).
"""
import csv
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import ListedColormap

CSV_PATH = Path(__file__).with_name("per_frame_swap.csv")
LOG_PATH = Path(__file__).with_name("data") / "network.log"
OUT_PATH = Path(__file__).with_name("first_drop.png")
RUNS_OUT_PATH = Path(__file__).with_name("first_drop_runs.png")
PHY_SHARE_OUT_PATH = Path(__file__).with_name("first_drop_phy_share.png")

MS_MAX = 500
N_MS = MS_MAX + 1
SWEEP_RESTART_WAIT = 480
EXPECTED_WAITS = set(range(N_MS))

# Streak buckets — inclusive lower bound; upper bound is the next entry's
# lower bound minus 1.
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

# Stack order: worst (drops) at the bottom → best (longest streak) at top.
STACK_ORDER = ["drop_first", "drop_other"] + [b[0] for b in STREAK_BUCKETS]

# Color scale: diverging red→yellow→green via RdYlGn (the conventional
# "error" / "good-bad" scale). drops live at the deep-red end; streaks
# walk from red through yellow to green as the streak grows.
_cmap = plt.get_cmap("RdYlGn")
_n_colors = len(STACK_ORDER)
COLORS = {c: _cmap(i / (_n_colors - 1)) for i, c in enumerate(STACK_ORDER)}
# Override drop colors: first drop white, subsequent drop black so they
# stand out unambiguously against the RdYlGn streak ramp.
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
    "Z": "#1f77b4",
    "H": "#ff7f0e",
}
PHY_LABELS = {
    "Z": "zigbee",
    "H": "halow",
}


def parse_line(line: str):
    parts = line.strip().split(",")
    if not parts:
        return None
    tag = parts[0]
    try:
        if tag == "Z" and len(parts) >= 6:
            return int(parts[2]), int(parts[1]), int(parts[3])
        if tag == "H" and len(parts) >= 7:
            return int(parts[3]), int(parts[2]), int(parts[4])
        if tag == "Z" and len(parts) >= 5:
            return None, int(parts[1]), int(parts[2])
        if tag == "H" and len(parts) >= 6:
            return None, int(parts[2]), int(parts[3])
    except ValueError:
        return None
    return None


def load_exact_rows_from_csv(path: Path):
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
                wait = int(row["wait_ms"])
            except (KeyError, TypeError, ValueError):
                continue
            if run < 0 or step < 0 or wait < 0:
                continue
            rows.append((run, step, wait))
            tag = row.get("tag", "")
            if tag in PHY_LABELS:
                per_run = phy_counts_by_run.setdefault(run, {"Z": 0, "H": 0})
                per_run[tag] += 1
    return rows, phy_counts_by_run


def load_legacy_rows_from_log(path: Path):
    rows = []
    with path.open() as f:
        for raw in f:
            parsed = parse_line(raw)
            if parsed is None:
                continue
            run, step, wait = parsed
            if run is not None:
                rows.append((run, step, wait))
            else:
                rows.append((step, wait))
    return rows


def streak_bucket_key(run_len: int) -> str:
    for key, lo, hi, _ in STREAK_BUCKETS:
        if lo <= run_len <= hi:
            return key
    return STREAK_BUCKETS[-1][0]


def run_length_in_set(m: int, s: set) -> int:
    if m not in s:
        return 0
    length = 1
    k = m + 1
    while k in s:
        length += 1
        k += 1
    k = m - 1
    while k in s:
        length += 1
        k -= 1
    return length


def classify_bucket(bucket: dict) -> np.ndarray:
    categories = np.full(N_MS, CATEGORY_INDEX["drop_other"], dtype=int)
    recv = bucket["received_ms"]
    miss = bucket["missed_ms_set"]
    first_miss = bucket["first_missed_ms"]

    for wait_ms in recv:
        categories[wait_ms] = CATEGORY_INDEX[streak_bucket_key(run_length_in_set(wait_ms, recv))]

    if first_miss is not None and first_miss in miss:
        categories[first_miss] = CATEGORY_INDEX["drop_first"]

    return categories


def plot_aggregate_view(fracs, counts, total_runs):
    x = np.arange(N_MS)
    bottom = np.zeros(N_MS)
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

    ax.set_xlim(-0.5, N_MS - 0.5)
    ax.set_ylim(0, 100.0)
    ax.set_xlabel("wait_ms (TX inter-frame delay; sweep counts down 500 → 0)")
    ax.set_ylabel("% of runs")
    ax.set_title(f"per_frame_swap reception by streak length — {total_runs} run(s)")
    ax.legend(loc="upper center", bbox_to_anchor=(0.5, -0.12), ncol=4, fontsize=8)
    ax.grid(True, axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(OUT_PATH, dpi=130, bbox_inches="tight")
    return fig


def plot_runs_view(classified_runs: np.ndarray, total_runs: int, mode: str):
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
    ax.set_xlim(-0.5, N_MS - 0.5)
    ax.set_xlabel("wait_ms (TX inter-frame delay; sweep counts down 500 → 0)")
    ax.set_ylabel("run index" if mode == "exact" else "sweep index")
    ax.set_title("per_frame_swap reception by run — unmerged stacked rows")

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


def plot_phy_share_view(phy_share_rows: np.ndarray, run_ids: list[int], mode: str):
    fig, ax = plt.subplots(figsize=(14, 5.5))
    x = np.arange(len(run_ids))
    bottom = np.zeros(len(run_ids))

    for phy in ("Z", "H"):
        values = phy_share_rows[:, 0] if phy == "Z" else phy_share_rows[:, 1]
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
    ax.set_ylim(0.0, 100.0)
    ax.set_xlabel("run" if mode == "exact" else "sweep")
    ax.set_ylabel("% of received packets")
    ax.set_title("per_frame_swap receive mix by run — Zigbee vs HaLow")
    ax.legend(loc="upper center", bbox_to_anchor=(0.5, -0.12), ncol=2)
    ax.grid(True, axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(PHY_SHARE_OUT_PATH, dpi=130, bbox_inches="tight")
    return fig


def build_exact_runs(rows: list[tuple[int, int, int]]):
    received_by_run: dict[int, set[int]] = {}
    inconsistent_waits = 0

    for run, step, wait in rows:
        if not (0 <= step <= MS_MAX and 0 <= wait <= MS_MAX):
            continue
        if wait != MS_MAX - step:
            inconsistent_waits += 1
        received_by_run.setdefault(run, set()).add(wait)

    if not received_by_run:
        return [], 0, 0

    run_start = min(received_by_run)
    run_stop = max(received_by_run)
    runs = []
    for run in range(run_start, run_stop + 1):
        received_ms = received_by_run.get(run, set())
        missed_ms_set = EXPECTED_WAITS - received_ms
        first_missed_ms = max(missed_ms_set) if missed_ms_set else None
        runs.append(
            {
                "run": run,
                "received_ms": received_ms,
                "missed_ms_set": missed_ms_set,
                "first_missed_ms": first_missed_ms,
            }
        )
    return runs, inconsistent_waits, len(received_by_run)


def build_phy_share_rows(phy_counts_by_run: dict[int, dict[str, int]], run_ids: list[int]) -> np.ndarray:
    rows = []
    for run_id in run_ids:
        counts = phy_counts_by_run.get(run_id, {"Z": 0, "H": 0})
        total = counts["Z"] + counts["H"]
        if total == 0:
            rows.append((0.0, 0.0))
        else:
            rows.append((100.0 * counts["Z"] / total, 100.0 * counts["H"] / total))
    return np.asarray(rows, dtype=float)


def build_legacy_sweeps(rows: list[tuple[int, int]]):
    sweeps = []
    current = {
        "received_ms": set(),
        "missed_ms_set": set(),
        "first_missed_ms": None,
    }
    prev_step = None
    prev_wait = None

    def push_current():
        if current["received_ms"] or current["missed_ms_set"]:
            sweeps.append(current.copy())

    for step, wait in rows:
        new_sweep = False
        if prev_wait is not None and wait > SWEEP_RESTART_WAIT and prev_wait <= SWEEP_RESTART_WAIT:
            new_sweep = True
        elif prev_step is not None and step <= prev_step:
            new_sweep = True

        if new_sweep:
            push_current()
            current = {
                "received_ms": set(),
                "missed_ms_set": set(),
                "first_missed_ms": None,
            }
            prev_step = None
            prev_wait = None

        if prev_step is not None and step > prev_step + 1:
            for missing_step in range(prev_step + 1, step):
                missing_wait = prev_wait - (missing_step - prev_step)
                if not (0 <= missing_wait <= MS_MAX):
                    continue
                if current["first_missed_ms"] is None:
                    current["first_missed_ms"] = missing_wait
                current["missed_ms_set"].add(missing_wait)

        if 0 <= wait <= MS_MAX:
            current["received_ms"].add(wait)

        prev_step = step
        prev_wait = wait

    push_current()
    return sweeps


def main():
    exact_rows: list[tuple[int, int, int]] = []
    legacy_rows: list[tuple[int, int]] = []
    source_path = None
    phy_counts_by_run: dict[int, dict[str, int]] = {}

    if CSV_PATH.exists():
        exact_rows, phy_counts_by_run = load_exact_rows_from_csv(CSV_PATH)
        source_path = CSV_PATH

    if not exact_rows:
        if not LOG_PATH.exists():
            sys.exit(f"missing {CSV_PATH} and {LOG_PATH} — run `per_frame_swap` first")
        source_path = LOG_PATH
        loaded_rows = load_legacy_rows_from_log(LOG_PATH)
        for row in loaded_rows:
            if len(row) == 3:
                exact_rows.append(row)
            else:
                legacy_rows.append(row)

    if exact_rows:
        runs, inconsistent_waits, runs_with_hits = build_exact_runs(exact_rows)
        total_runs = len(runs)
        if total_runs == 0:
            sys.exit(f"{source_path} has no valid runs")
        mode = "exact"
        if inconsistent_waits:
            print(
                f"warning: {inconsistent_waits} row(s) had wait_ms != {MS_MAX} - step; plotting by wait_ms"
            )
        print(
            f"using explicit run counters from {source_path.name}: {runs_with_hits} run(s) with receptions, {total_runs} total run(s) in range"
        )
        buckets = runs
        run_ids = [bucket["run"] for bucket in buckets]
    else:
        buckets = build_legacy_sweeps(legacy_rows)
        total_runs = len(buckets)
        mode = "legacy"
        run_ids = list(range(total_runs))

    if total_runs == 0:
        sys.exit(f"{source_path} has no sweeps")

    counts = {c: np.zeros(N_MS, dtype=int) for c in STACK_ORDER}
    classified_runs = []

    for bucket in buckets:
        bucket_classes = classify_bucket(bucket)
        classified_runs.append(bucket_classes)
        for category in STACK_ORDER:
            counts[category] += (bucket_classes == CATEGORY_INDEX[category]).astype(int)

    totals = sum(counts[c] for c in STACK_ORDER)
    if mode == "exact":
        if not np.all(totals == total_runs):
            sys.exit("internal error: exact run accounting did not classify every wait_ms bin")
        # Percent, not a 0-1 fraction: every rate on these plots reads in %.
        fracs = {c: 100.0 * counts[c] / total_runs for c in STACK_ORDER}
    else:
        safe_totals = np.where(totals == 0, 1, totals)
        fracs = {c: 100.0 * counts[c] / safe_totals for c in STACK_ORDER}

    classified_runs = np.vstack(classified_runs)
    plot_aggregate_view(fracs, counts, total_runs)
    plot_runs_view(classified_runs, total_runs, mode)
    if mode == "exact":
        phy_share_rows = build_phy_share_rows(phy_counts_by_run, run_ids)
        plot_phy_share_view(phy_share_rows, run_ids, mode)
    plt.show()
    print(f"wrote {OUT_PATH}")
    print(f"wrote {RUNS_OUT_PATH}")
    if mode == "exact":
        print(f"wrote {PHY_SHARE_OUT_PATH}")
    print(f"runs: {total_runs}")
    if mode == "legacy":
        print("mode: legacy step-gap inference (log has no run field)")
    else:
        print("mode: exact run-based accounting")
    for c in STACK_ORDER:
        print(f"  {LABELS[c]:25s} total events: {int(counts[c].sum())}")


if __name__ == "__main__":
    main()
