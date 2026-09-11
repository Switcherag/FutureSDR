#!/usr/bin/env python3
r"""Build an alternating-PHY cf32 stream from the two cropped reference frames.

One file per inter-frame spacing. Each file is the same 1000-frame script --
H, Z, H, Z, ... -- with `--ifs` milliseconds of gap between consecutive frames,
so across a sweep the ONLY thing that changes is how much time the receiver is
given to swap PHYs before the next frame lands. That is the quantity the PER
curve in ../../plot/per_soft.svg is plotted against.

Layout of a generated file:

    [pad] H [ifs] Z [ifs] H [ifs] Z ... H [ifs] Z [pad]
           \____ 1000 frames, strictly alternating ____/

`--pad` is lead-in/lead-out only. It is deliberately NOT applied around every
frame: a per-frame guard would add to every gap and silently shift the IFS
axis the whole experiment is measured on.

Because both reference frames are single recordings replayed verbatim, every
copy of a frame is byte-identical -- the sequence numbers and firmware stamps
inside them repeat. PER therefore cannot be recovered from those fields, and
is instead counted against the manifest this writes: exactly `n/2` H and `n/2`
Z were sent, so PER per PHY is `1 - received/sent`.

Usage:
    ./gen_iq.py --ifs 2.5 -o bench/iq/ifs_2.500.cf32
    ./gen_iq.py --ifs 2.5 --frames 1000 --pad 0.1 -o ...
"""

import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
REC = os.path.dirname(HERE)          # .../recording


def load_frame(path):
    x = np.fromfile(path, dtype=np.complex64)
    if x.size == 0:
        sys.exit(f"{path}: empty or unreadable as cf32")
    meta_path = os.path.splitext(path)[0] + ".meta.json"
    meta = {}
    try:
        with open(meta_path) as fh:
            meta = json.load(fh)
    except OSError:
        pass
    return x, meta


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--halow", default=os.path.join(REC, "halow_frame.cf32"),
                    help="cropped 802.11ah reference frame (cf32)")
    ap.add_argument("--zigbee", default=os.path.join(REC, "zigbee_frame.cf32"),
                    help="cropped 802.15.4 reference frame (cf32)")
    ap.add_argument("--ifs", type=float, required=True,
                    help="inter-frame spacing in milliseconds")
    ap.add_argument("--frames", type=int, default=1000,
                    help="total frames in the file, alternating H/Z (default 1000)")
    ap.add_argument("--pad", type=float, default=0.1,
                    help="lead-in/lead-out silence in milliseconds (default 0.1)")
    ap.add_argument("--lead-in", type=float, default=0.0,
                    help="EXTRA silence before the first frame, milliseconds. "
                         "Covers receiver startup: the head flowgraph is a "
                         "permanent and so starts before the swappable PHY "
                         "flow, and anything it streams in between is "
                         "discarded by a bridge that is not connected yet. "
                         "Without it the first frame of every file is lost for "
                         "a reason that has nothing to do with the swap. Not "
                         "part of any inter-frame gap, so it does not touch "
                         "the IFS axis.")
    ap.add_argument("--sample-rate", type=float, default=4e6,
                    help="must match both frames and the flows (default 4e6)")
    ap.add_argument("--start", choices=["H", "Z"], default="H",
                    help="which PHY transmits first (default H)")
    ap.add_argument("--pattern", default=None,
                    help="transmit pattern as a string of H/Z, repeated to fill "
                         "--frames. Default is the alternating 'HZ' starting at "
                         "--start. Give a single letter for a same-PHY stream: "
                         "'Z' is the all-802.15.4 script whose recorded "
                         "counterpart is zigbee_swapfinal.csv, 'H' the "
                         "all-802.11ah one behind halow_swapv6*.csv. The "
                         "receiver still swaps on every frame -- between the A "
                         "and B flows of that one PHY.")
    ap.add_argument("--noise-dbfs", type=float, default=None,
                    help="fill gaps with complex noise at this level instead of "
                         "exact zeros; see the warning this prints when unset")
    ap.add_argument("--noise-from", default=None, metavar="CF32",
                    help="fill the gaps with REAL recorded noise sampled from the "
                         "quiet parts of this capture, instead of zeros or "
                         "synthetic Gaussian. This is the faithful option: white "
                         "Gaussian has none of the receiver's filter roll-off, DC "
                         "offset or spurs, and it is exactly the front-end shape "
                         "that halowv6A's normalised |MA96|/MA128 detector sees "
                         "between bursts. Measured floor of the raw captures is "
                         "about -54 dBFS.")
    ap.add_argument("--seed", type=int, default=0, help="noise RNG seed")
    ap.add_argument("-o", "--out", required=True, help="output cf32 path")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    h, h_meta = load_frame(args.halow)
    z, z_meta = load_frame(args.zigbee)

    # Refuse to interleave two captures made at different rates: the IFS axis
    # is in milliseconds, and a rate mismatch turns it into a lie.
    for tag, meta in (("halow", h_meta), ("zigbee", z_meta)):
        rate = meta.get("sample_rate_hz")
        if rate is not None and abs(rate - args.sample_rate) > 1.0:
            sys.exit(f"{tag} frame was captured at {rate} Hz, not {args.sample_rate} Hz")

    fs = args.sample_rate
    n_ifs = int(round(args.ifs * 1e-3 * fs))
    n_pad = int(round(args.pad * 1e-3 * fs))
    n_lead = int(round(args.lead_in * 1e-3 * fs))

    if args.pattern:
        order = [c for c in args.pattern.upper() if c in "HZ"]
        if not order:
            sys.exit(f"--pattern {args.pattern!r} has no H or Z in it")
    else:
        order = [args.start, "Z" if args.start == "H" else "H"]
    labels = [order[i % len(order)] for i in range(args.frames)]
    frames = [h if lab == "H" else z for lab in labels]

    # Total length: pads, every frame, and a gap after every frame but the last.
    total = n_lead + 2 * n_pad + sum(f.size for f in frames) + n_ifs * max(0, args.frames - 1)

    if args.noise_from:
        src = np.fromfile(args.noise_from, dtype=np.complex64)
        if src.size == 0:
            sys.exit(f"--noise-from {args.noise_from}: empty or not cf32")
        # Quiet samples only: anything at or below the 40th percentile of power
        # is between bursts. Taking the whole file would splice fragments of
        # real frames into the gaps, which the receiver would then try to decode.
        pw = np.abs(src) ** 2
        quiet = src[pw <= np.percentile(pw, 40)]
        if quiet.size < 1000:
            sys.exit(f"--noise-from {args.noise_from}: too little quiet signal")
        rng = np.random.default_rng(args.seed)
        # Draw a random contiguous run per fill so the splice keeps the noise's
        # own correlation structure rather than shuffling it into whiteness.
        start = rng.integers(0, max(1, quiet.size - total % quiet.size))
        out = np.resize(np.roll(quiet, -int(start)), total).astype(np.complex64)
        if not args.quiet:
            floor = 10 * np.log10(float((np.abs(quiet) ** 2).mean()) + 1e-30)
            print(f"gaps filled with real noise from {os.path.basename(args.noise_from)} "
                  f"({quiet.size} quiet samples, {floor:.1f} dBFS)", file=sys.stderr)
    elif args.noise_dbfs is None:
        out = np.zeros(total, dtype=np.complex64)
        if not args.quiet:
            print("WARNING: gaps are exact zeros. halowv6A.toml's `div_mag` computes "
                  "|MA96(x·conj(x_d))| / MA128(|x|²), which is 0/0 on a silent gap. "
                  "If the 802.11ah trace collapses, re-generate with e.g. "
                  "--noise-dbfs -55 (the measured floor of the raw captures).",
                  file=sys.stderr)
    else:
        rng = np.random.default_rng(args.seed)
        # dBFS is on total complex power, so each quadrature gets sigma/sqrt(2).
        sigma = (10 ** (args.noise_dbfs / 20.0)) / np.sqrt(2.0)
        out = (rng.normal(0.0, sigma, total)
               + 1j * rng.normal(0.0, sigma, total)).astype(np.complex64)

    manifest = []
    pos = n_lead + n_pad
    for i, (f, lab) in enumerate(zip(frames, labels)):
        out[pos:pos + f.size] = f
        manifest.append({"i": i, "phy": lab, "start": int(pos), "len": int(f.size)})
        pos += f.size
        if i != args.frames - 1:
            pos += n_ifs
    assert pos + n_pad == total, (pos, n_pad, n_lead, total)

    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    out.tofile(args.out)

    n_h = labels.count("H")
    n_z = labels.count("Z")
    meta = {
        "format": "cf32",
        "sample_rate_hz": fs,
        "ifs_ms": args.ifs,
        "pad_ms": args.pad,
        "lead_in_ms": args.lead_in,
        "frames": args.frames,
        "sent_H": n_h,
        "sent_Z": n_z,
        "start_phy": labels[0] if labels else args.start,
        "pattern": "".join(order),
        "noise_dbfs": args.noise_dbfs,
        "samples": int(total),
        "duration_s": float(total / fs),
        "halow_frame": {"path": os.path.basename(args.halow), "samples": int(h.size)},
        "zigbee_frame": {"path": os.path.basename(args.zigbee), "samples": int(z.size)},
        "frames_manifest": manifest,
    }
    with open(os.path.splitext(args.out)[0] + ".meta.json", "w") as fh:
        json.dump(meta, fh, indent=2)
        fh.write("\n")

    if not args.quiet:
        print(f"ifs {args.ifs:6.3f} ms  frames {args.frames} (H={n_h} Z={n_z})  "
              f"{total} samples  {total/fs:7.3f} s  {total*8/2**20:8.1f} MiB  -> {args.out}")


if __name__ == "__main__":
    main()
