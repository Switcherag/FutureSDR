#!/usr/bin/env python3
r"""Sweep inter-frame spacing: generate an IQ file, replay it, record the PER.

For every IFS step this does three things and then throws the samples away:

    gen_iq.py --ifs X      ->  bench/iq/ifs_X.cf32   (~40..350 MiB)
    ziglow_replay --iq ...  ->  bench/csv/ifs_X.csv   (one row per decoded frame)
    count rows by PHY       ->  one row in the results CSV

The IQ is deleted after each step unless `--keep`. Kept, the full default
sweep is about 19.6 GiB; the sweep itself only ever needs one file at a time.

PER is a count, not an inference. Both reference frames are single recordings
replayed verbatim, so every copy carries the same stamp and the same sequence
number and no field inside a frame can tell two transmissions apart. What the
generator's sidecar does provide is exactly how many of each PHY were sent, so

    PER_phy = 1 - received_phy / sent_phy

with `received_phy` the number of `rx` rows the replay attributed to that PHY.
A decode that produces more frames than were sent (a duplicate) would push the
rate below zero; that is reported rather than clamped, because it means the
receiver is double-counting and the run should not be trusted.

RUNS ARE SERIAL, DELIBERATELY. The replay paces samples on the wall clock, so
two of them on one machine compete for CPU and each makes the other look worse
at swapping. `--jobs` does not exist for that reason.

Usage:
    ./run_sweep.py                          # 10.0 -> 0.1 ms, 100 steps
    ./run_sweep.py --ifs-start 5 --ifs-stop 0.5 --ifs-step -0.5 --frames 200
    ./run_sweep.py --resume                 # skip steps already in the results
"""

import argparse
import csv
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EXAMPLE = os.path.dirname(os.path.dirname(HERE))          # .../real_device_swap
REPO = os.path.dirname(os.path.dirname(EXAMPLE))          # repo root
DEFAULT_BIN = os.path.join(REPO, "target", "release", "ziglow_replay")

FIELDS = ["ifs_ms", "sent_H", "rx_H", "per_H_pct", "sent_Z", "rx_Z", "per_Z_pct",
          "swaps", "avg_swap_ms", "overruns", "wall_s"]


def ifs_steps(start, stop, step):
    if step == 0:
        sys.exit("--ifs-step cannot be 0")
    vals, x = [], start
    # Accumulate by index rather than repeated addition: 10.0 - 0.1*n stays
    # exact to the printed precision where `x += -0.1` drifts into 2.9999997
    # and produces a duplicate filename two steps later.
    n = 0
    while (step < 0 and x >= stop - 1e-9) or (step > 0 and x <= stop + 1e-9):
        vals.append(round(x, 6))
        n += 1
        x = round(start + step * n, 6)
    return vals


def count_frames(path):
    """Received frames per PHY, and mean swap time, from a replay CSV."""
    rx = {"H": 0, "Z": 0}
    swaps, swap_ms = 0, 0.0
    with open(path, newline="") as fh:
        for row in csv.DictReader(fh):
            if row.get("frame_event") == "rx" and row.get("phy") in rx:
                rx[row["phy"]] += 1
            try:
                swap_ms += float(row["swap_ms"])
                swaps += 1
            except (KeyError, TypeError, ValueError):
                pass
    return rx, swaps, (swap_ms / swaps if swaps else 0.0)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    # 4 -> 0, not 10 -> 0.1: every curve is flat on the same ~1% floor above
    # about 3 ms, so the old range spent most of a 23-minute sweep measuring
    # nothing. The whole breakdown region is under 2 ms.
    ap.add_argument("--ifs-start", type=float, default=4.0, help="ms (default 4.0)")
    ap.add_argument("--ifs-stop", type=float, default=0.0, help="ms (default 0.0)")
    ap.add_argument("--ifs-step", type=float, default=-0.1, help="ms (default -0.1)")
    ap.add_argument("--frames", type=int, default=1000, help="frames per step (default 1000)")
    ap.add_argument("--pad", type=float, default=0.1, help="lead-in/out ms (default 0.1)")
    ap.add_argument("--lead-in", type=float, default=300.0,
                    help="extra silence before the first frame, ms (default 300). "
                         "Covers receiver startup, since the head flowgraph starts "
                         "before the swappable PHY flow. Not part of any IFS gap.")
    ap.add_argument("--noise-dbfs", type=float, default=None,
                    help="gap noise floor; passed through to gen_iq.py")
    ap.add_argument("--start-phy", choices=["H", "Z"], default="H")
    ap.add_argument("--pattern", default=None,
                    help="transmit pattern, passed to gen_iq.py. 'Z' or 'H' "
                         "makes a same-PHY sweep; pair it with --flow-a/--flow-b "
                         "for that PHY's A and B flows.")
    ap.add_argument("--flow-a", default=None, help="replay --flow-a")
    ap.add_argument("--flow-b", default=None, help="replay --flow-b")
    ap.add_argument("--bin", default=DEFAULT_BIN, help="ziglow_replay binary")
    ap.add_argument("--iq-dir", default=os.path.join(HERE, "iq"))
    ap.add_argument("--csv-dir", default=os.path.join(HERE, "csv"))
    ap.add_argument("--out", default=os.path.join(HERE, "results", "per_replay.csv"))
    ap.add_argument("--log", default=os.path.join(HERE, "results", "replay.log"),
                    help="replay stdout/stderr goes here; the per-swap timing "
                         "breakdown is far too noisy for a 100-step sweep")
    ap.add_argument("--keep", action="store_true", help="keep the generated IQ files")
    ap.add_argument("--resume", action="store_true", help="skip steps already in --out")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        sys.exit(f"{args.bin} not found — build it first:\n"
                 f"  cargo build --release -p real-device-swap-example --bin ziglow_replay")

    for d in (args.iq_dir, args.csv_dir, os.path.dirname(args.out), os.path.dirname(args.log)):
        os.makedirs(d, exist_ok=True)

    steps = ifs_steps(args.ifs_start, args.ifs_stop, args.ifs_step)

    done = set()
    if args.resume and os.path.exists(args.out):
        with open(args.out, newline="") as fh:
            done = {round(float(r["ifs_ms"]), 6) for r in csv.DictReader(fh)}
        steps = [s for s in steps if s not in done]
        print(f"resuming: {len(done)} step(s) already done, {len(steps)} to go")

    new_file = not (args.resume and os.path.exists(args.out))
    out_fh = open(args.out, "w" if new_file else "a", newline="")
    w = csv.DictWriter(out_fh, fieldnames=FIELDS)
    if new_file:
        w.writeheader()
        out_fh.flush()

    log = open(args.log, "w" if new_file else "a")
    # Estimated replay time: every step is `frames * (mean frame + ifs)` of
    # wall clock, and the mean frame is ~1.07 ms for the H/Z pair.
    est = sum((args.lead_in + args.frames * (1.07 + s)) / 1000.0 for s in steps)
    print(f"{len(steps)} step(s), ~{est/60:.1f} min of replay plus per-step startup\n")

    t_start = time.time()
    for i, ifs in enumerate(steps, 1):
        tag = f"ifs_{ifs:07.3f}"
        iq = os.path.join(args.iq_dir, tag + ".cf32")
        rep_csv = os.path.join(args.csv_dir, tag + ".csv")

        gen = [sys.executable, os.path.join(HERE, "gen_iq.py"),
               "--ifs", str(ifs), "--frames", str(args.frames),
               "--pad", str(args.pad), "--lead-in", str(args.lead_in),
               "--start", args.start_phy,
               "-o", iq, "--quiet"]
        if args.noise_dbfs is not None:
            gen += ["--noise-dbfs", str(args.noise_dbfs)]
        if args.pattern:
            gen += ["--pattern", args.pattern]
        subprocess.run(gen, check=True)

        with open(os.path.splitext(iq)[0] + ".meta.json") as fh:
            meta = json.load(fh)

        t0 = time.time()
        log.write(f"\n===== ifs {ifs} ms =====\n"); log.flush()
        cmd = [args.bin, "--iq", iq, "--csv", rep_csv,
               "--start-phy", meta.get("start_phy", args.start_phy),
               "--sample-rate", str(meta["sample_rate_hz"])]
        if args.flow_a:
            cmd += ["--flow-a", args.flow_a]
        if args.flow_b:
            cmd += ["--flow-b", args.flow_b]
        proc = subprocess.run(cmd, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True)
        log.write(proc.stdout or ""); log.flush()
        wall = time.time() - t0
        if proc.returncode != 0:
            sys.exit(f"replay failed at ifs {ifs} (exit {proc.returncode}); see {args.log}")

        # The replay reports overruns on its last line; a non-zero count means
        # the flowgraph fell behind real time and the point is not comparable.
        # The head's BridgeSink reports its own drops on stderr, as
        # "bridge BridgeSinkC32: dropped N items (M total)". A non-zero total
        # means the receiving flowgraph did not keep up with real time, so that
        # step's PER is not attributable to the swap.
        overruns = 0
        for line in (proc.stdout or "").splitlines():
            if "dropped" in line and "bridge" in line.lower():
                try:
                    overruns = int(line.rsplit("(", 1)[1].split()[0])
                except (IndexError, ValueError):
                    pass

        rx, swaps, avg_swap = count_frames(rep_csv)
        sent_h, sent_z = meta["sent_H"], meta["sent_Z"]
        per_h = 100.0 * (1 - rx["H"] / sent_h) if sent_h else float("nan")
        per_z = 100.0 * (1 - rx["Z"] / sent_z) if sent_z else float("nan")

        w.writerow({"ifs_ms": ifs, "sent_H": sent_h, "rx_H": rx["H"],
                    "per_H_pct": round(per_h, 4), "sent_Z": sent_z, "rx_Z": rx["Z"],
                    "per_Z_pct": round(per_z, 4), "swaps": swaps,
                    "avg_swap_ms": round(avg_swap, 4), "overruns": overruns,
                    "wall_s": round(wall, 2)})
        out_fh.flush()

        if not args.keep:
            # The replay writes a per-step head TOML beside the IQ (it is the
            # checked-in samples_head.toml with its FileSourceC32 path pointed
            # at this step's file), so it goes with the samples.
            for p in (iq, os.path.splitext(iq)[0] + ".meta.json",
                      os.path.splitext(iq)[0] + ".head.toml"):
                try:
                    os.remove(p)
                except OSError:
                    pass

        elapsed = time.time() - t_start
        eta = elapsed / i * (len(steps) - i)
        flag = "  !! OVERRUN" if overruns else ""
        fmt = lambda got, sent, per: (f"{got:4d}/{sent} ({per:6.2f}%)"
                                      if sent else f"{'-':>4}/{sent}    (  --  )")
        print(f"[{i:3d}/{len(steps)}] ifs {ifs:6.3f} ms   "
              f"H {fmt(rx['H'], sent_h, per_h)}   "
              f"Z {fmt(rx['Z'], sent_z, per_z)}   "
              f"swap {avg_swap:5.3f} ms   {wall:6.1f} s   ETA {eta/60:5.1f} min{flag}")

    out_fh.close()
    log.close()
    print(f"\nresults -> {args.out}")


if __name__ == "__main__":
    main()
