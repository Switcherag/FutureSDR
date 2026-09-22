"""The swaps of bench.sh: their CSV key, label, colour (categorical slots
1-6 in order, validated with the dataviz method's script) and line style;
every figure draws a swap with the same colour."""

SERIES = {
    "zz": ("ZigBee → ZigBee", "#2a78d6", "-", "o"),
    "ss": ("HaLow simple → simple", "#eb6834", "--", "s"),
    "sz": ("HaLow simple ⇄ ZigBee", "#1baf7a", "-.", "^"),
    "gg": ("HaLow granular → granular", "#eda100", ":", "D"),
    "gd": ("HaLow granular, decoder only (inverse ⇄ Viterbi)", "#e87ba4", (0, (5, 1, 1, 1)), "v"),
    "11": ("HaLow single block → single", "#008300", (0, (3, 1, 1, 1, 1, 1)), "P"),
}
SURFACE, GRID, AXIS = "#fcfcfb", "#e1e0d9", "#c3c2b7"
INK, INK2, MUTED = "#0b0b0b", "#52514e", "#898781"


def load(path, xmax=None):
    """IFS (true, frame end to next frame start), PER % and median swap ms
    of a bench CSV, up to `xmax` ms."""
    import csv

    with open(path) as f:
        key = "ifs_true_ms" if "ifs_true_ms" in f.readline() else "ifs_ms"
    rows = sorted(csv.DictReader(open(path)), key=lambda r: float(r[key]))
    if xmax is not None:
        rows = [r for r in rows if float(r[key]) <= xmax]
    return (
        [float(r[key]) for r in rows],
        [100 * float(r["per"]) for r in rows],
        [float(r["swap_median_ms"]) for r in rows],
    )


def style(ax):
    ax.set_facecolor(SURFACE)
    ax.grid(True, color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(AXIS)
    ax.tick_params(colors=INK2, labelsize=10)
