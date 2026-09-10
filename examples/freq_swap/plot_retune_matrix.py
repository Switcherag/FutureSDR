#!/usr/bin/env python3
"""Square heatmap of channel-to-channel retune latency from `retune_matrix`.

Rows are the channel tuned *from*, columns the channel tuned *to*, and the cell
is how long `set_frequency` took to make that hop. Both channel plans are on
both axes, so the matrix has four quadrants and they answer different questions:

    Z→Z   in-band hops inside 2.4 GHz
    H→H   in-band hops inside 902-928 MHz
    Z→H   cross-band, the swap a dual-PHY receiver actually performs
    H→Z   cross-band, the return leg

If cross-band is intrinsically expensive, the two off-diagonal quadrants light
up uniformly. If instead the cost is contention with a running RX stream, the
whole matrix rises together and the quadrant structure stays flat — which is
why it is worth running with and without `--stream`.

Cells are drawn square (`aspect="equal"`) so distances read honestly.

Usage:
    python3 plot_retune_matrix.py [csv ...] [--out plot.png] [--log]

Several CSVs are shown side by side on a shared colour scale, which is how to
compare idle vs streaming, or two sample rates, without eyeballing two legends.
"""

import argparse
import csv
import statistics
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import LogNorm, Normalize

MUTED = "#898781"
INK = "#0b0b0b"

# `--paper` restyles for a journal figure rather than a screen, the same way
# `real_device_swap/plot/plot_per_compare.py` does — kept as its own copy
# because these example scripts each stand alone. Latin Modern Roman is the
# Computer Modern LaTeX sets body text in, so the figure matches its page.
PAPER_RC = {
    "font.family": "serif",
    "font.serif": ["Latin Modern Roman", "Nimbus Roman", "Times New Roman",
                   "DejaVu Serif"],
    "mathtext.fontset": "cm",
    "font.size": 9,
    "axes.labelsize": 9,
    "axes.titlesize": 9,
    "xtick.labelsize": 6,
    "ytick.labelsize": 6,
    "axes.linewidth": 0.6,
    "svg.fonttype": "none",
    "pdf.fonttype": 42,  # TrueType, not Type 3 — most journals reject Type 3
    "ps.fonttype": 42,
}


def load(path):
    """Return `(labels, matrix)` with matrix[i][j] = ms for labels[i] → labels[j]."""
    with path.open(newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        sys.exit(f"{path}: empty")

    labels = []
    for r in rows:  # preserve capture order, which is the channel-plan order
        for k in ("from_label", "to_label"):
            if r[k] not in labels:
                labels.append(r[k])
    index = {lab: i for i, lab in enumerate(labels)}

    m = np.full((len(labels), len(labels)), np.nan)
    for r in rows:
        m[index[r["from_label"]], index[r["to_label"]]] = float(r["ms"])
    return labels, m


def plot(captures, out, use_log, paper=False, figsize=None, stack=False):
    finite = np.concatenate([m[np.isfinite(m)].ravel() for _, _, m in captures])
    if finite.size == 0:
        sys.exit("no finite measurements")
    lo = max(finite.min(), 1e-3) if use_log else finite.min()
    norm = LogNorm(vmin=lo, vmax=finite.max()) if use_log else Normalize(finite.min(), finite.max())

    # Stacking is what lets the figure be narrow. Two square matrices side by
    # side in one column give each about an inch and a half, at which the
    # channel labels are unreadable; one above the other keeps each panel wide
    # enough while the type stays at its 9 pt, which is what makes it read
    # large against the axes.
    if figsize is None:
        if paper:
            figsize = ((3.4, 1.1 + 2.5 * len(captures)) if stack
                       else (1.0 + 2.9 * len(captures), 3.4))
        else:
            figsize = (1.5 + 6.2 * len(captures), 7.0)
    grid = (len(captures), 1) if stack else (1, len(captures))
    fig, axes = plt.subplots(*grid, figsize=figsize,
                             layout="constrained", squeeze=False)
    axes = axes.reshape(-1)[None, :]  # one flat row, whichever way it was laid out
    if not paper:
        fig.patch.set_facecolor("#fcfcfb")
    ink = INK if paper else MUTED

    for ax, (label, labels, m) in zip(axes[0], captures):
        im = ax.imshow(m, cmap="magma_r", norm=norm, aspect="equal",
                       interpolation="nearest", origin="upper")
        ax.set_title(label, color=INK, loc="left",
                     fontsize=9 if paper else 12)
        ax.set_xlabel("Retune to" if paper else "retune to", color=ink)
        ax.set_ylabel("Retune from" if paper else "retune from", color=ink)

        step = max(1, len(labels) // (10 if paper else 21))
        ticks = range(0, len(labels), step)
        ax.set_xticks(list(ticks), [labels[i] for i in ticks], rotation=90)
        ax.set_yticks(list(ticks), [labels[i] for i in ticks])
        ax.tick_params(colors=ink, length=2 if paper else 0,
                       width=0.6, labelsize=6 if paper else 7)

        # No quadrant divider on a paper figure. A line at the plan boundary
        # is wider than the gap between two cells at column width, so it clips
        # the cells either side of it — and the four quadrants are already
        # obvious as blocks of colour, which is the finding itself.
        if not paper:
            cut = next((i for i, l in enumerate(labels) if l[0] != labels[0][0]), None)
            if cut:
                for pos in (cut - 0.5,):
                    ax.axhline(pos, color="#1f6fd0", linewidth=1.2)
                    ax.axvline(pos, color="#1f6fd0", linewidth=1.2)
        for spine in ax.spines.values():
            spine.set_visible(paper)
            spine.set_color(INK)

    cbar = fig.colorbar(im, ax=axes[0], shrink=0.82, pad=0.02)
    cbar.set_label("Retune duration (ms)" if paper
                   else "set_frequency duration (ms)", color=ink)
    cbar.ax.tick_params(colors=ink, labelsize=7 if paper else None)
    cbar.outline.set_visible(False)
    if use_log:
        # A log bar defaults to decade ticks, which here means two labels and
        # no way to read a quadrant off the scale. Label the round numbers that
        # actually fall in range instead.
        ticks = [t for t in (1, 2, 5, 10, 20, 50, 100, 200)
                 if lo <= t <= finite.max()]
        cbar.set_ticks(ticks)
        cbar.set_ticklabels([str(t) for t in ticks])
        cbar.ax.minorticks_off()

    if out:
        if paper:
            fig.savefig(out, bbox_inches="tight", pad_inches=0.02, dpi=600,
                        transparent=True)
        else:
            fig.savefig(out, dpi=150)
        print(f"wrote {out}")
    else:
        plt.show()


def hop_stats(labels, m, plans):
    """`{hop_name: [ms]}` — in-band per plan, and cross-band as one population.

    The diagonal is excluded. A `set_frequency` to the channel the radio is
    already on is not a hop: it costs about half a real one (5.5 ms against
    10.1 ms with host tuning), and leaving it in would drag every in-band mean
    down by a case a swapping receiver never performs.
    """
    letters = [lab[0] for lab in labels]
    out = {}
    for a in sorted(set(letters)):
        name = f"In-band, {plans.get(a, a)}"
        out[name] = [
            m[i][j]
            for i, la in enumerate(letters) if la == a
            for j, lb in enumerate(letters) if lb == a and i != j
            if np.isfinite(m[i][j])
        ]
    out["Cross-band"] = [
        m[i][j]
        for i, la in enumerate(letters)
        for j, lb in enumerate(letters) if la != lb
        if np.isfinite(m[i][j])
    ]
    return {k: v for k, v in out.items() if v}


def latex_table(captures, plans, out):
    """An IEEE-style table of mean retune latency per hop type, per capture.

    Caption above the table and `\\arraystretch` loosened, as IEEEtran wants;
    booktabs for the rules, which IEEE templates accept and which reads better
    than `\\hline` at this density. A speed-up column is added when exactly two
    captures are compared, since that is the whole point of running two.
    """
    rows = [(title, hop_stats(labels, m, plans)) for title, labels, m in captures]
    names = list(rows[0][1])
    compare = len(rows) == 2

    lines = [
        "% Requires \\usepackage{booktabs}",
        "\\begin{table}[!t]",
        "  \\renewcommand{\\arraystretch}{1.2}",
        "  \\caption{Retune Latency by Hop Type}",
        "  \\label{tab:retune-latency}",
        "  \\centering",
        "  \\small",
        "  \\begin{tabular}{l" + "rr" * len(rows) + ("r" if compare else "") + "}",
        "    \\toprule",
        "    " + "".join(f"& \\multicolumn{{2}}{{c}}{{{t}}} " for t, _ in rows).rstrip()
        + (" &" if compare else "") + " \\\\",
        "    " + " ".join(f"\\cmidrule(lr){{{2 + 2 * i}-{3 + 2 * i}}}"
                        for i in range(len(rows))),
        "    Hop type " + "".join("& Mean & $\\sigma$ " for _ in rows)
        + ("& Gain " if compare else "") + "\\\\",
        "    \\midrule",
    ]

    for name in names:
        cells, means = [], []
        for _title, stats in rows:
            values = stats.get(name, [])
            if not values:
                cells.append("& \\multicolumn{2}{c}{---} ")
                means.append(None)
                continue
            mean = statistics.fmean(values)
            sigma = statistics.stdev(values) if len(values) > 1 else float("nan")
            means.append(mean)
            cells.append(f"& {mean:.2f} & {sigma:.2f} ")
        gain = ""
        if compare and all(means) and means[1]:
            gain = f"& {means[0] / means[1]:.1f}$\\times$"
        lines.append(f"    {name} " + "".join(cells).rstrip() + " " + gain + " \\\\")

    counts = ", ".join(f"{n} ($n={len(rows[0][1][n])}$)" for n in names)
    lines += [
        "    \\bottomrule",
        "  \\end{tabular}",
        # `\\par` first, or the note runs on from the tabular instead of
        # starting a line; `\\parbox{\\linewidth}` then holds it to the column
        # width rather than letting it set to the full text width.
        "  \\par\\vspace{2pt}",
        "  \\parbox{\\linewidth}{\\footnotesize All times in ms. Hops to the "
        "channel already tuned are excluded: they are not retunes and cost "
        "about half of one. Measurements per hop type: " + counts + ".}",
        "\\end{table}",
    ]
    text = "\n".join(lines) + "\n"
    if out:
        out.write_text(text)
        print(f"wrote {out}")
    else:
        print(text)


def quadrants(labels, m):
    """Median ms for each (from-plan, to-plan) quadrant."""
    plans = [l[0] for l in labels]
    out = {}
    for a in sorted(set(plans)):
        for b in sorted(set(plans)):
            rows = [i for i, p in enumerate(plans) if p == a]
            cols = [j for j, p in enumerate(plans) if p == b]
            block = m[np.ix_(rows, cols)]
            block = block[np.isfinite(block)]
            if block.size:
                out[f"{a}->{b}"] = (np.median(block), block.min(), block.max())
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("csv", nargs="*", type=Path,
                   help="one or more retune_matrix CSVs, shown side by side")
    p.add_argument("--labels", help="comma-separated titles, one per CSV")
    p.add_argument("--out", type=Path, help="save instead of opening a window")
    p.add_argument("--log", action="store_true",
                   help="log colour scale — use when idle and stalled hops differ by decades")
    p.add_argument("--paper", action="store_true",
                   help="journal styling: serif type, boxed frame, tight "
                        "column-width panels")
    p.add_argument("--figsize", nargs=2, type=float, metavar=("W", "H"),
                   help="figure size in inches; the default follows --paper")
    p.add_argument("--latex", nargs="?", type=Path, const=None, default=False,
                   metavar="TEX",
                   help="emit an IEEE-style table of mean retune latency per "
                        "hop type, to this file or stdout; no figure is drawn")
    p.add_argument("--plan", action="append", default=[], metavar="LETTER=NAME",
                   help="PHY name for a channel-plan prefix, e.g. Z=802.15.4; "
                        "repeatable")
    p.add_argument("--stack", action="store_true",
                   help="panels in one column instead of side by side, which "
                        "is what makes a one-column figure legible")
    args = p.parse_args()

    paths = args.csv or [Path("retune_matrix.csv")]
    for path in paths:
        if not path.exists():
            sys.exit(f"missing {path} — run retune_matrix first")
    titles = args.labels.split(",") if args.labels else [p.stem for p in paths]

    captures = []
    for path, title in zip(titles and paths, titles):
        labels, m = load(path)
        captures.append((title, labels, m))
        print(f"== {title}: {len(labels)} channels")
        for name, (med, lo, hi) in quadrants(labels, m).items():
            print(f"   {name}: median {med:8.3f} ms   min {lo:8.3f}   max {hi:8.3f}")

    if args.latex is not False:
        plans = {}
        for pair in args.plan:
            letter, _, name = pair.partition("=")
            if not name:
                sys.exit(f"--plan wants LETTER=NAME, got {pair!r}")
            plans[letter] = name
        latex_table(captures, plans, args.latex)
        return

    if args.paper:
        plt.rcParams.update(PAPER_RC)
    plot(captures, args.out, args.log, args.paper,
         tuple(args.figsize) if args.figsize else None, args.stack)


if __name__ == "__main__":
    main()
