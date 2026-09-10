#!/usr/bin/env python3
"""Per-step statistics for a `softswap_latency` capture.

The benchmark writes one row per swap and one column per swap step, so the
question this answers is where a swap's time actually goes — and how much of
what is left, once the radio is out of the loop, is one step.

    read_toml            read + parse the incoming flow's TOML, for [radio]
    disconnect           gate the head off the incoming flowgraph's channels
    retune_radio         apply the new flow's [radio] demand (nothing, on a
                         null head — that is the point of running against one)
    park_selectors       route permanent-FG selectors away
    load_plugins         dlopen any plugin .so not already resident
    build_read_toml      read + parse the same TOML again, to build from
    build_create_blocks  ask each plugin for a block and add it
    build_connect        wire the declared connections and taps
    build_bridge_ports   inject the bridge blocks joining this FG to its
                         neighbours, and connect them
    start_runtime        hand the flowgraph to the runtime: spawn, initialise
    reconnect            clear the deque and re-open the gates
    terminate_spawn      hand the outgoing flowgraph to a background thread
    unpark_selectors     route the selectors back

`build_and_start` and `total` are rollups — sums of steps above, kept for
continuity with coarser captures and never charted beside their own parts.

A step that did not run is written as -1 by the benchmark rather than 0, and
is reported as "not run" rather than averaged in as a real measurement of zero.

Two things are worth reading beyond the medians. The **tail**, because a swap
that is usually fast and occasionally slow drops frames on the occasions; p95
and max are printed for every step and the distribution is plotted. And
**direction**, because Z→H and H→Z build different flowgraphs — if one is
consistently dearer, that is the flowgraph, not the swap machinery.

Usage:
    python3 softswap_stats.py [csv] [--out plot.png] [--paper] [--by-direction]

Defaults to ../softswap_latency.csv and an interactive window.
"""

import argparse
import csv
import statistics
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib.pyplot as plt

# The leaf steps, in the order they run. Disjoint — none contains another — so
# they can share one axis. `build_and_start` and `total` are rollups of the
# steps above them and are handled apart: charting a sum beside its own parts
# invites reading it as another part.
STEPS = [
    "read_toml", "disconnect", "retune_radio", "park_selectors",
    "load_plugins", "build_read_toml", "build_create_blocks", "build_connect",
    "build_bridge_ports", "start_runtime", "reconnect", "terminate_spawn",
    "unpark_selectors",
]
ROLLUPS = ["build_and_start", "total"]

# The five steps that together are `build_and_start`, so the summary can say
# what share of a swap goes into standing the incoming flowgraph up.
BUILD_STEPS = [
    "build_read_toml", "build_create_blocks", "build_connect",
    "build_bridge_ports", "start_runtime",
]

# Readable names for the LaTeX table. The CSV's identifiers are fine in a
# terminal but belong in neither a paper's table nor its \texttt, and every
# underscore in them would have to be escaped anyway.
TEX_LABEL = {
    "read_toml": "Parse TOML (for \\texttt{[radio]})",
    "disconnect": "Disconnect head",
    "retune_radio": "Retune radio",
    "park_selectors": "Park selectors",
    "load_plugins": "Load plugins (\\texttt{dlopen})",
    "build_read_toml": "Re-parse TOML (to build from)",
    "build_create_blocks": "Create blocks",
    "build_connect": "Wire connections",
    "build_bridge_ports": "Inject bridge blocks",
    "start_runtime": "Start on runtime",
    "reconnect": "Reconnect head",
    "terminate_spawn": "Detach old flowgraph",
    "unpark_selectors": "Unpark selectors",
    "build_and_start": "Build and start",
    "total": "Total",
}

# Which capture direction is which switch. `from`/`to` in the CSV are the PHY
# letters the benchmark writes; what a reader wants is the PHY being switched
# *to*, since that is the flowgraph being stood up.
TEX_DIRECTION = {
    "H\u2192Z": "Switching to 802.15.4",
    "Z\u2192H": "Switching to 802.11ah",
}

SURFACE = "#fcfcfb"
MUTED = "#898781"
GRID = "#e1e0d9"
INK = "#0b0b0b"
FAINT = "#c3c2b7"
BASE = "#2a78d6"
TOTAL = "#eb6834"

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
    "svg.fonttype": "none",
    "pdf.fonttype": 42,
    "ps.fonttype": 42,
}


def load(path):
    """`({step: [ms]}, {direction: [total_ms]})` from a benchmark CSV."""
    with path.open(newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        sys.exit(f"{path}: no swaps recorded")

    if "total" not in rows[0]:
        sys.exit(f"{path}: not a softswap_latency CSV — no total column")
    # Take whichever steps this capture recorded rather than demanding all of
    # them, so a CSV written before the build phase was broken out still reads.
    present = [c for c in STEPS + ROLLUPS if c in rows[0]]

    steps = defaultdict(list)
    by_direction = defaultdict(list)
    per_direction = defaultdict(lambda: defaultdict(list))
    for row in rows:
        direction = (f"{row['from']}→{row['to']}"
                     if row.get("from") and row.get("to") else None)
        for name in present:
            value = float(row[name])
            if value < 0:  # -1 marks a step that did not run
                continue
            steps[name].append(value)
            if direction:
                per_direction[direction][name].append(value)
        if direction:
            by_direction[direction].append(float(row["total"]))
    return steps, by_direction, per_direction, len(rows)


def quantile(values, q):
    """Linear-interpolated quantile of an already-sorted list."""
    pos = q * (len(values) - 1)
    lo = int(pos)
    hi = min(lo + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (pos - lo)


def summarise(steps, by_direction, n_swaps, path):
    print(f"== {path.name}: {n_swaps} swaps")
    print(f"\n{'step':<22}{'n':>5}{'median':>9}{'mean':>9}{'p95':>9}"
          f"{'p99':>9}{'min':>9}{'max':>9}{'share':>8}   (* = rollup)")

    total_median = statistics.median(steps["total"]) if steps["total"] else 0.0
    for name in STEPS + ROLLUPS:
        values = sorted(steps.get(name, []))
        if not values:
            continue
        median = quantile(values, 0.5)
        # Share of the median swap. It will not sum to 100%: these are medians
        # of separate distributions, not one swap's parts, and the bookkeeping
        # between steps belongs to none of them.
        share = f"{100 * median / total_median:.1f}%" if total_median and name != "total" else ""
        label = name + "*" if name in ROLLUPS else name
        print(
            f"{label:<22}{len(values):>5}{median:>9.3f}"
            f"{statistics.fmean(values):>9.3f}{quantile(values, 0.95):>9.3f}"
            f"{quantile(values, 0.99):>9.3f}{values[0]:>9.3f}{values[-1]:>9.3f}"
            f"{share:>8}"
        )

    build = [statistics.median(steps[n]) for n in BUILD_STEPS if steps.get(n)]
    if build and total_median:
        print(f"\nstanding the incoming flowgraph up is {sum(build):.3f} ms of the "
              f"{total_median:.3f} ms median swap ({100 * sum(build) / total_median:.0f}%)")
    parses = [statistics.median(steps[n]) for n in ("read_toml", "build_read_toml")
              if steps.get(n)]
    if len(parses) == 2 and total_median:
        print(f"the same TOML is read and parsed twice, {parses[0]:.3f} + "
              f"{parses[1]:.3f} ms ({100 * sum(parses) / total_median:.0f}% of a swap)")

    if len(by_direction) > 1:
        print("\ntotal by direction — a gap here is the flowgraph, not the swap")
        for name, values in sorted(by_direction.items()):
            values = sorted(values)
            print(f"  {name:<8}{len(values):>5} swaps   median {quantile(values, 0.5):7.3f} ms"
                  f"   p95 {quantile(values, 0.95):7.3f} ms")


def fmt(value):
    """Three decimals, except that a figure below the last place is reported as
    a bound rather than as a zero it never measured."""
    if value != value:
        return "---"
    if 0 < value < 0.0005:
        return "$<$0.001"
    return f"{value:.3f}"


def latex_table(per_direction, out):
    """A booktabs table of mean and standard deviation, one group per switch.

    Two columns per direction, so it fits a single-column `table` rather than
    needing the full width. Sigma rather than sigma-squared: it is in the same
    unit as the mean and can be read against it directly, where the variance at
    this scale lands around 1e-6 ms^2 and needs scaling before it means
    anything.

    Needs \\usepackage{booktabs}; nothing else — no siunitx, no array.
    """
    directions = sorted(per_direction)
    if not directions:
        sys.exit("no direction column in this capture — nothing to tabulate")

    rows_present = [n for n in STEPS + ROLLUPS
                    if any(per_direction[d].get(n) for d in directions)]
    counts = ", ".join(
        f"{TEX_DIRECTION.get(d, d)} $n={len(per_direction[d]['total'])}$"
        for d in directions
    )

    lines = [
        "% Requires \\usepackage{booktabs}",
        "\\begin{table}[t]",
        "  \\centering",
        "  \\small",
        "  \\caption{Cost of each step of a PHY swap, measured on a null head so no "
        "radio retune is in the loop. All figures in ms, as mean and standard "
        "deviation over the swaps of that direction. Rows marked $\\dagger$ are "
        "rollups of the rows above them and are not additional steps. "
        + counts + ".}",
        "  \\label{tab:softswap-latency}",
        "  \\begin{tabular}{l" + "rr" * len(directions) + "}",
        "    \\toprule",
    ]

    header = ["    "]
    for i, d in enumerate(directions):
        header.append(f"& \\multicolumn{{2}}{{c}}{{{TEX_DIRECTION.get(d, d)}}} ")
    lines.append("".join(header).rstrip() + " \\\\")
    lines.append("    " + " ".join(
        f"\\cmidrule(lr){{{2 + 2 * i}-{3 + 2 * i}}}" for i in range(len(directions))
    ))
    lines.append("    Step " + "".join(
        "& {Mean} & {$\\sigma$} " for _ in directions
    ).rstrip() + " \\\\")
    lines.append("    \\midrule")

    for name in rows_present:
        if name in ROLLUPS:
            lines.append("    \\midrule")
        label = TEX_LABEL.get(name, name.replace("_", "\\_"))
        if name in ROLLUPS:
            label += "$^\\dagger$"
        cells = []
        for d in directions:
            values = sorted(per_direction[d].get(name, []))
            if not values:
                cells.append("& \\multicolumn{2}{c}{---} ")
                continue
            # Sample standard deviation needs two points; one measurement has
            # no spread to report rather than a spread of zero.
            sigma = statistics.stdev(values) if len(values) > 1 else float("nan")
            cells.append(
                "& " + " & ".join(fmt(v) for v in (statistics.fmean(values), sigma)) + " "
            )
        lines.append(f"    {label} " + "".join(cells).rstrip() + " \\\\")

    lines += ["    \\bottomrule", "  \\end{tabular}", "\\end{table}"]
    text = "\n".join(lines) + "\n"

    if out:
        out.write_text(text)
        print(f"\nwrote {out}")
    else:
        print("\n" + text)


def plot(steps, by_direction, n_swaps, title, out, paper, split):
    fig, (ax, ax2) = plt.subplots(
        2, 1, figsize=(3.5, 4.6) if paper else (10, 7),
        layout="constrained", gridspec_kw={"height_ratios": [3, 2]},
    )
    if not paper:
        fig.patch.set_facecolor(SURFACE)

    # Panel 1: where the time goes. One box per step, log x because the steps
    # span four decades — a linear axis shows build_and_start and nothing else.
    ran = [name for name in STEPS if steps.get(name)]  # leaves only, never rollups
    data = [steps[name] for name in ran]
    box = ax.boxplot(data, vert=False, widths=0.6, whis=(1, 99), showfliers=False,
                     patch_artist=True, medianprops={"color": INK, "linewidth": 1.2})
    for patch in box["boxes"]:
        patch.set_facecolor(BASE)
        patch.set_alpha(0.75)
        patch.set_edgecolor(BASE)
    ax.set_yticks(range(1, len(ran) + 1), [n.replace("_", " ") for n in ran])
    ax.set_xscale("log")
    ax.set_xlabel("Step duration (ms)" if paper else "step duration (ms)")
    ax.invert_yaxis()  # first step at the top, in the order they run

    # Panel 2: the total, which is what a dropped frame actually experiences.
    if split and len(by_direction) > 1:
        for i, (name, values) in enumerate(sorted(by_direction.items())):
            ax2.hist(values, bins=40, histtype="step", linewidth=1.3,
                     color=[BASE, TOTAL][i % 2], label=f"{name} (n={len(values)})")
        ax2.legend(frameon=paper, fontsize=8, labelcolor=None if paper else MUTED,
                   edgecolor="#b0b0b0" if paper else None)
    else:
        ax2.hist(steps["total"], bins=40, color=TOTAL, alpha=0.85)
    ax2.set_xlabel("Total swap latency (ms)" if paper else "total swap latency (ms)")
    ax2.set_ylabel("Swaps" if paper else "swaps")

    if steps["total"]:
        ordered = sorted(steps["total"])
        for q, style in ((0.5, "-"), (0.95, "--")):
            value = quantile(ordered, q)
            ax2.axvline(value, color=INK, linewidth=0.9, linestyle=style, alpha=0.7)
            ax2.annotate(f"p{int(q * 100)} {value:.2f} ms", xy=(value, 0),
                         xytext=(3, 6), textcoords="offset points",
                         rotation=90, fontsize=7, color=INK if paper else MUTED)

    for a in (ax, ax2):
        if paper:
            a.grid(True, color="#d8d8d8", linewidth=0.4, linestyle=(0, (1, 2)))
            for spine in a.spines.values():
                spine.set_visible(True)
                spine.set_color(INK)
        else:
            a.set_facecolor(SURFACE)
            a.grid(True, color=GRID, linewidth=0.8)
            a.tick_params(colors=MUTED)
            for side, spine in a.spines.items():
                spine.set_visible(side in ("left", "bottom"))
                spine.set_color(FAINT)
        a.set_axisbelow(True)

    if not paper:
        ax.set_title(f"{title} — {n_swaps} swaps", color=INK, loc="left", fontsize=13)

    if out:
        if paper:
            fig.savefig(out, bbox_inches="tight", pad_inches=0.02, dpi=600,
                        transparent=True)
        else:
            fig.savefig(out, dpi=150)
        print(f"\nwrote {out}")
    else:
        plt.show()


def main():
    here = Path(__file__).resolve().parent
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="?", type=Path,
                   default=here.parent / "softswap_latency.csv")
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--paper", action="store_true",
                   help="journal styling: serif type, boxed frame, column width")
    p.add_argument("--by-direction", action="store_true",
                   help="split the total histogram by swap direction")
    p.add_argument("--latex", nargs="?", type=Path, const=None, default=False,
                   metavar="TEX",
                   help="emit a booktabs table of per-step statistics per swap "
                        "direction, to this file or stdout; no figure is drawn")
    args = p.parse_args()

    if not args.csv.exists():
        sys.exit(f"missing {args.csv} — run `softswap_latency` first")
    if args.paper and not args.out:
        sys.exit("--paper is for a saved figure; give --out too")

    steps, by_direction, per_direction, n_swaps = load(args.csv)
    summarise(steps, by_direction, n_swaps, args.csv)

    if args.latex is not False:
        latex_table(per_direction, args.latex)
        return

    if args.paper:
        plt.rcParams.update(PAPER_RC)
    plot(steps, by_direction, n_swaps,
         f"{args.csv.stem} — swap latency by step", args.out, args.paper,
         args.by_direction)


if __name__ == "__main__":
    main()
