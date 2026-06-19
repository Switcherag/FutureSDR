#!/usr/bin/env python3
"""Analyse the SDR-side channel-swap timing recorded by `zigbee_swap`.

No RF is transmitted: every loop iteration hits the 800 ms RX_TIMEOUT and
swaps the Zigbee channel anyway, so each swap exercises the real
`ctrl.swap()` (flowgraph teardown + rebuild + hardware retune).

Two distinct costs are separated here:

  * BLOCKING swap  — `[swap] total` (== CSV `swap_ms`): the time ctrl.swap()
    blocks the control loop. This is what bounds how fast you can switch.
    It breaks down into 6 instrumented sub-steps:
        0-retune_radio  1-park_selectors  2-terminate
        3-load_plugins  4-build_and_start 5-unpark_selectors
    ('other' = total - sum(sub-steps): scheduler / await overhead).

  * ASYNC hw settle — `[fast retune] freq F MHz settled in X ms`: how long
    the bladeRF actually takes to land on the new frequency. Runs on a
    background thread, overlapped with RX, so it does NOT block the swap.

Inputs (alongside this script):
    zigbee_swap_run.log   console capture (per-step breakdown + hw settle)
    zigbee_swap.csv       one row per swap (rx_t_ms, swap_ms)  [cross-check]

Output:
    swap_timing.png       breakdown + settle + distribution plots
    a stats table on stdout
"""
import csv
import re
import statistics as stats
from pathlib import Path

import matplotlib.pyplot as plt

HERE = Path(__file__).parent
LOG_PATH = HERE / "zigbee_swap_run.log"
CSV_PATH = HERE / "zigbee_swap.csv"
OUT_PATH = HERE / "swap_timing.png"

# Ordered sub-steps that compose the blocking swap total.
SUBSTEPS = [
    "retune_radio",
    "park_selectors",
    "terminate",
    "load_plugins",
    "build_and_start",
    "unpark_selectors",
]

RE_EVENT = re.compile(r"^\[t=([\d.]+)ms\]\s+\[(timeout|rx)\b.*?(?:on|tap=\S+)?\s*([AB])?")
RE_DISPATCH = re.compile(r"\[retune\] dispatched \(fast path\) in ([\d.]+) ms")
RE_SUBSTEP = re.compile(r"\[swap\] \d-(\w+):\s+([\d.]+) ms")
RE_TOTAL = re.compile(r"\[swap\] total:\s+([\d.]+) ms")
RE_SETTLE = re.compile(r"\[fast retune\] freq ([\d.]+) MHz settled in ([\d.]+) ms")


def parse_log(path):
    """Return a list of swap dicts parsed from the console log."""
    swaps = []
    cur = None

    def flush():
        if cur is not None:
            swaps.append(cur)

    for line in path.read_text().splitlines():
        m = RE_EVENT.match(line)
        if m:
            flush()
            cur = {
                "rx_t_ms": float(m.group(1)),
                "event": m.group(2),
                "phy_from": m.group(3),
                "sub": {},
                "dispatch_ms": None,
                "total_ms": None,
                "settle_mhz": None,
                "settle_ms": None,
            }
            continue
        if cur is None:
            continue  # startup lines before the first swap
        m = RE_DISPATCH.search(line)
        if m:
            cur["dispatch_ms"] = float(m.group(1))
            continue
        m = RE_SUBSTEP.search(line)
        if m:
            cur["sub"][m.group(1)] = float(m.group(2))
            continue
        m = RE_TOTAL.search(line)
        if m:
            cur["total_ms"] = float(m.group(1))
            continue
        m = RE_SETTLE.search(line)
        if m:
            cur["settle_mhz"] = float(m.group(1))
            cur["settle_ms"] = float(m.group(2))
            continue
    flush()
    # keep only fully-formed swaps (have a total)
    return [s for s in swaps if s["total_ms"] is not None]


def load_csv(path):
    if not path.exists():
        return []
    with path.open() as f:
        return list(csv.DictReader(f))


def pct(sorted_vals, q):
    """Linear-interpolated percentile of a pre-sorted list (q in 0..100)."""
    if not sorted_vals:
        return float("nan")
    if len(sorted_vals) == 1:
        return sorted_vals[0]
    rank = (q / 100.0) * (len(sorted_vals) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(sorted_vals) - 1)
    frac = rank - lo
    return sorted_vals[lo] * (1 - frac) + sorted_vals[hi] * frac


def describe(name, vals):
    s = sorted(vals)
    return {
        "name": name,
        "n": len(s),
        "min": s[0],
        "median": stats.median(s),
        "mean": stats.fmean(s),
        "p95": pct(s, 95),
        "max": s[-1],
        "std": stats.pstdev(s) if len(s) > 1 else 0.0,
    }


def print_table(rows):
    hdr = f"{'component':<22}{'n':>4}{'min':>9}{'median':>9}{'mean':>9}{'p95':>9}{'max':>9}{'std':>9}"
    print(hdr)
    print("-" * len(hdr))
    for r in rows:
        print(
            f"{r['name']:<22}{r['n']:>4}"
            f"{r['min']:>9.3f}{r['median']:>9.3f}{r['mean']:>9.3f}"
            f"{r['p95']:>9.3f}{r['max']:>9.3f}{r['std']:>9.3f}"
        )


def main():
    if not LOG_PATH.exists():
        raise SystemExit(f"missing {LOG_PATH} — run zigbee_swap and capture stdout first")

    swaps = parse_log(LOG_PATH)
    if not swaps:
        raise SystemExit("no swaps parsed from log")

    csv_rows = load_csv(CSV_PATH)

    totals = [s["total_ms"] for s in swaps]
    settles = [s["settle_ms"] for s in swaps if s["settle_ms"] is not None]
    sub_series = {k: [s["sub"].get(k, 0.0) for s in swaps] for k in SUBSTEPS}
    # 'other' = total minus the instrumented sub-steps (scheduler/await overhead)
    other = [
        max(0.0, s["total_ms"] - sum(s["sub"].get(k, 0.0) for k in SUBSTEPS))
        for s in swaps
    ]

    # ---- stats table -------------------------------------------------------
    print(f"\nParsed {len(swaps)} swaps from {LOG_PATH.name}"
          f"  ({len(csv_rows)} rows in {CSV_PATH.name})\n")
    rows = [describe(k, sub_series[k]) for k in SUBSTEPS]
    rows.append(describe("other(overhead)", other))
    rows.append(describe("BLOCKING total", totals))
    rows.append(describe("hw settle (async)", settles))
    print_table(rows)

    steady = totals[1:] if len(totals) > 1 else totals
    print(
        f"\nBlocking swap: first(cold)={totals[0]:.3f} ms, "
        f"steady median={stats.median(steady):.3f} ms  "
        f"-> the cost that bounds switch rate."
    )
    print(
        f"Hw retune settle: median={stats.median(settles):.3f} ms "
        f"(async on bg thread, overlapped with RX -> does NOT block the swap)."
    )

    # ---- plots -------------------------------------------------------------
    idx = list(range(len(swaps)))
    fig, (ax1, ax2, ax3) = plt.subplots(3, 1, figsize=(11, 13))

    # Panel 1: stacked breakdown of the blocking swap, per swap index.
    bottoms = [0.0] * len(swaps)
    cmap = plt.get_cmap("tab10")
    for i, k in enumerate(SUBSTEPS):
        ax1.bar(idx, sub_series[k], bottom=bottoms, width=0.9,
                color=cmap(i), label=k)
        bottoms = [b + v for b, v in zip(bottoms, sub_series[k])]
    ax1.bar(idx, other, bottom=bottoms, width=0.9,
            color="lightgrey", label="other (overhead)")
    ax1.set_title("Blocking swap cost per swap — ctrl.swap() control-loop block")
    ax1.set_xlabel("swap index")
    ax1.set_ylabel("time (ms)")
    ax1.legend(ncol=4, fontsize=8, loc="upper right")
    ax1.grid(True, axis="y", alpha=0.3)

    # Panel 2: async hw retune settle, coloured by target frequency.
    by_freq = {}
    for s in swaps:
        if s["settle_ms"] is not None:
            by_freq.setdefault(s["settle_mhz"], ([], []))
            by_freq[s["settle_mhz"]][0].append(idx[swaps.index(s)])
            by_freq[s["settle_mhz"]][1].append(s["settle_ms"])
    for freq, (xs, ys) in sorted(by_freq.items()):
        ax2.plot(xs, ys, marker="o", linestyle="-", markersize=4,
                 label=f"{freq:.0f} MHz")
    med = stats.median(settles)
    ax2.axhline(med, color="k", linestyle="--", linewidth=1,
                label=f"median {med:.1f} ms")
    ax2.set_title("Hardware retune settle per swap — async on bg thread (overlapped, non-blocking)")
    ax2.set_xlabel("swap index")
    ax2.set_ylabel("settle time (ms)")
    ax2.legend(fontsize=8)
    ax2.grid(True, alpha=0.3)

    # Panel 3: per-component distribution (blocking parts only, linear scale).
    box_data = [sub_series[k] for k in SUBSTEPS] + [other, totals]
    box_labels = SUBSTEPS + ["other", "TOTAL\n(block)"]
    ax3.boxplot(box_data, showfliers=True, widths=0.6)
    ax3.set_xticklabels(box_labels, rotation=30, ha="right", fontsize=8)
    ax3.set_title("Per-component distribution (linear scale)")
    ax3.set_ylabel("time (ms)")
    ax3.grid(True, axis="y", alpha=0.3)

    fig.tight_layout()
    fig.savefig(OUT_PATH, dpi=130)
    print(f"\nwrote {OUT_PATH}")
    plt.show()


if __name__ == "__main__":
    main()
