#!/usr/bin/env python3
"""
Compare .so file sizes across dyn, dynv2, and stat methods.

Usage:
    python3 compare_sizes.py [output.png]
"""

import sys
import os
import matplotlib.pyplot as plt
import numpy as np

def get_size_kb(path):
    """Get file size in KB."""
    try:
        return os.path.getsize(path) / 1024
    except FileNotFoundError:
        print(f"WARNING: {path} not found")
        return 0

def main():
    output = sys.argv[1] if len(sys.argv) > 1 else "size_comparison.png"

    # Define files to compare
    files = {
        "dynv2\n(dylib+prefer-dynamic)": {
            "Plugin .so": "/home/alakhdar/Projets/DynLib10.0/FutureSDR/shared_libs/release/libincrement_plugin.so",
        },
        "dynv2\n(cdylib, old)": {
            "Plugin .so": None,  # placeholder, size provided manually
        },
        "dyn\n(old libloading)": {
            "Plugin .so": "/home/alakhdar/Projets/FutureSDR-Engine/Dylib-full-no-LTO copy/shared_libs/libincrement.so",
        },
    }

    # Get sizes
    sizes = {}
    for method, paths in files.items():
        for label, path in paths.items():
            if path:
                sizes[method] = get_size_kb(path)

    # Manually add old cdylib size (18MB from before)
    sizes["dynv2\n(cdylib, old)"] = 18 * 1024  # 18MB in KB

    methods = list(sizes.keys())
    values = [sizes[m] for m in methods]

    fig, ax = plt.subplots(1, 1, figsize=(10, 6))

    colors = ['#2196F3', '#FF9800', '#4CAF50']
    bars = ax.bar(range(len(methods)), values, color=colors, width=0.6)

    # Add size labels on bars
    for bar, val in zip(bars, values):
        if val >= 1024:
            label = f"{val/1024:.1f} MB"
        else:
            label = f"{val:.0f} KB"
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + max(values)*0.02,
                label, ha='center', va='bottom', fontweight='bold', fontsize=12)

    ax.set_ylabel("Size (KB)")
    ax.set_title("Increment Plugin .so Size Comparison")
    ax.set_xticks(range(len(methods)))
    ax.set_xticklabels(methods, fontsize=10)
    ax.grid(axis="y", alpha=0.3)

    plt.tight_layout()
    plt.savefig(output, dpi=150)
    print(f"Plot saved to {output}")
    plt.show()


if __name__ == "__main__":
    main()
