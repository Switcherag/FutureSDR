#!/usr/bin/env python3
"""Print the built size of every plugin in plugins/.

Walks each plugins/<crate>/Cargo.toml, resolves its [package] name, and
looks for the matching target/release/lib<name>.so. Reports a size table
sorted largest-first, plus which plugins did not produce a .so (build
failed or skipped — e.g. GPU/zynq plugins needing extra system deps).

Run from anywhere; paths resolve relative to this file (repo root).

    python3 plugin_sizes.py            # release (default)
    python3 plugin_sizes.py --debug    # look in target/debug instead
"""
import argparse
import re
from pathlib import Path

import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parent
PLUGINS_DIR = ROOT / "plugins"
OUT_PNG = ROOT / "plugin_sizes.png"

NAME_RE = re.compile(r'^\s*name\s*=\s*"([^"]+)"')


def package_name(cargo_toml):
    """Return the [package] name from a Cargo.toml (first name after [package])."""
    in_package = False
    for line in cargo_toml.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            m = NAME_RE.match(line)
            if m:
                return m.group(1)
    return None


def human(n):
    """Bytes -> short human string (KiB/MiB)."""
    if n >= 1024 * 1024:
        return f"{n / 1024 / 1024:.2f} MiB"
    return f"{n / 1024:.1f} KiB"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--debug", action="store_true",
                    help="inspect target/debug instead of target/release")
    args = ap.parse_args()

    profile = "debug" if args.debug else "release"
    target_dir = ROOT / "target" / profile
    if not target_dir.exists():
        raise SystemExit(f"no {target_dir} — build the plugins first")

    built = []   # (name, size_bytes, so_path)
    missing = []  # names with no .so
    for cargo in sorted(PLUGINS_DIR.glob("*/Cargo.toml")):
        name = package_name(cargo)
        if name is None:
            print(f"  warn: no [package] name in {cargo}")
            continue
        so = target_dir / f"lib{name}.so"
        if so.exists():
            built.append((name, so.stat().st_size, so))
        else:
            missing.append(name)

    built.sort(key=lambda r: r[1], reverse=True)

    width = max((len(n) for n, _, _ in built), default=20)
    print(f"\nPlugin sizes  (target/{profile})\n")
    print(f"{'plugin':<{width}}  {'size':>10}  {'bytes':>10}")
    print("-" * (width + 24))
    for name, size, _ in built:
        print(f"{name:<{width}}  {human(size):>10}  {size:>10}")

    total = sum(s for _, s, _ in built)
    print("-" * (width + 24))
    print(f"{'TOTAL ' + str(len(built)) + ' plugins':<{width}}  "
          f"{human(total):>10}  {total:>10}")
    if built:
        avg = total / len(built)
        print(f"{'avg':<{width}}  {human(avg):>10}")
        print(f"\nlargest : {built[0][0]}  ({human(built[0][1])})")
        print(f"smallest: {built[-1][0]}  ({human(built[-1][1])})")

    if missing:
        print(f"\nno .so built ({len(missing)}):")
        for name in missing:
            print(f"  - {name}")
    else:
        print("\nall plugin crates produced a .so")

    # ---- plot: horizontal bar chart, largest at top -----------------------
    if not built:
        return
    names = [n for n, _, _ in built]
    sizes_mib = [s / 1024 / 1024 for _, s, _ in built]
    ypos = range(len(names))

    fig, ax = plt.subplots(figsize=(11, max(6, 0.22 * len(names))))
    cmap = plt.get_cmap("viridis")
    smax = max(sizes_mib)
    ax.barh(list(ypos), sizes_mib,
            color=[cmap(s / smax) for s in sizes_mib])
    ax.set_yticks(list(ypos))
    ax.set_yticklabels(names, fontsize=7)
    ax.invert_yaxis()  # largest at top
    ax.set_xlabel("size (MiB)")
    ax.set_title(f"Plugin .so sizes (target/{profile}) — "
                 f"{len(built)} built, total {human(total)}")
    for y, s in zip(ypos, sizes_mib):
        ax.text(s, y, f" {s:.2f}", va="center", fontsize=6)
    ax.grid(True, axis="x", alpha=0.3)
    fig.tight_layout()
    fig.savefig(OUT_PNG, dpi=130)
    print(f"\nwrote {OUT_PNG}")
    plt.show()


if __name__ == "__main__":
    main()
