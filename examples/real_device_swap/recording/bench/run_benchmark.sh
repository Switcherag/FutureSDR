#!/usr/bin/env bash
# Reproduce the per-frame PHY-swap benchmark end to end and emit both figures:
#
#   plot/per_soft.png           swap DISCARDS the samples that arrive mid-swap
#   plot/per_soft_buffered.png  swap BUFFERS them (PLUGIN_HOST_SWAP_BUFFER=1)
#
# Six sweeps, 41 IFS steps each from 4.0 ms down to 0.0, 1000 frames per step:
# 802.15.4 A<->B, 802.11ah v6 A<->B, and the cross-PHY alternation, under each
# of the two swap policies. No radio is involved -- the stimulus is a recorded
# IQ file replayed through flows/samples_head.toml at 4 MSps.
#
#   bash recording/bench/run_benchmark.sh              # build + run + plot
#   bash recording/bench/run_benchmark.sh --plot-only  # replot existing results
#   SWEEP_FRAMES=200 bash ... run_benchmark.sh         # quick pass (~1/5 time)
#   EXTRA_CARGO_ARGS=--no-default-features bash ...    # host without libbladeRF
#
# Runtime is dominated by wall-clock replay, not CPU: each sweep replays about
# 2.3 minutes of signal, so the whole thing is ~20 min on any machine that can
# keep up with 4 MSps. If it cannot, the OVERRUN warnings below will say so.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EX="$(cd "$HERE/../.." && pwd)"              # examples/real_device_swap
cd "$EX"

FRAMES="${SWEEP_FRAMES:-1000}"
BIN=../../target/release/ziglow_replay
PLOT_ONLY=0
[ "${1:-}" = "--plot-only" ] && PLOT_ONLY=1

# ── Reference frames and the IFS correction ─────────────────────────────
# Each cropped reference frame carries a little noise either side of the PPDU.
# That padding sits INSIDE the frame file, so the true gap between two PPDUs is
# the labelled IFS plus one frame's trailing residual and the next frame's
# leading one. The x axis is corrected at plot time by that residual, computed
# here from the theoretical PPDU duration rather than hard-coded:
#
#   802.15.4  ESP32-C6, ch15, O-QPSK 250 kbit/s, PSDU 36 B
#             = (4 preamble + 1 SFD + 1 PHR + 36) B x 8 bit / 4 bit/sym x 16 us
#             = 1344 us
#   802.11ah  MM6108, S1G ch34, 2 MHz, MCS0, PSDU 30 B
#             = 6 preamble symbols + ceil((16+240+6)/26) data symbols, 40 us each
#             = 680 us
read -r SHIFT_Z SHIFT_H SHIFT_X <<<"$(python3 - <<'PY'
import numpy as np, math
Z_TH = (4+1+1+36)*8/4*16.0                      # us
Ts, NSD = 40.0, 52
H_TH = 6*Ts + math.ceil((16+30*8+6)/(NSD*1*0.5))*Ts
out=[]
for tag, th in (("zigbee", Z_TH), ("halow", H_TH)):
    n = np.fromfile(f"recording/{tag}_frame.cf32", dtype=np.complex64).size
    out.append(n/4e6*1e6 - th)                  # residual, us
z, h = out
print(f"{z/1000:.6f} {h/1000:.6f} {(z+h)/2/1000:.6f}")   # ms; cross alternates
PY
)"
echo "IFS residual correction: 802.15.4 +${SHIFT_Z} ms, 802.11ah +${SHIFT_H} ms, cross +${SHIFT_X} ms"

# ── Build ───────────────────────────────────────────────────────────────
if [ "$PLOT_ONLY" -eq 0 ]; then
  echo; echo "=== build (ABI-safe: whole package set in one invocation) ==="
  # NEVER `cargo build -p <one plugin>` -- cargo re-resolves and the resulting
  # plugin .so fails to load against libfuturesdr.so. build.sh guards this.
  bash build.sh 2>&1 | tail -3
  [ -x "$BIN" ] || { echo "FATAL: $BIN missing after build"; exit 1; }
fi

# ── Sweeps ──────────────────────────────────────────────────────────────
# Gap fill per PHY, from measurement: the 802.11ah v6 detector normalises by
# MA(|x|^2) and wants a realistic noise floor between bursts; 802.15.4 has no
# such stage and measures better on digital silence.
sweep() {                 # $1 outdir  $2 tag  $3.. extra args
  local out="$1" tag="$2"; shift 2
  echo "--- $tag -> $out ---"
  python3 -u recording/bench/run_sweep.py --frames "$FRAMES" \
      --out "$out/per_replay_$tag.csv" --log "$out/replay_$tag.log" "$@"
}

run_set() {               # $1 outdir  $2 "buffered"|"discard"
  local R="recording/bench/results/$1"; mkdir -p "$R"
  if [ "$2" = "buffered" ]; then export PLUGIN_HOST_SWAP_BUFFER=1
  else unset PLUGIN_HOST_SWAP_BUFFER || true; fi
  echo; echo "########## $2 set ##########"
  sweep "$R" z2z   --pattern Z --flow-a flows/zigbee_rxA.toml --flow-b flows/zigbee_rxB.toml
  sweep "$R" h2h   --pattern H --flow-a flows/halowv6A.toml   --flow-b flows/halowv6B.toml \
                   --noise-from recording/halow_raw.cf32
  sweep "$R" cross --noise-from recording/halow_raw.cf32
  unset PLUGIN_HOST_SWAP_BUFFER || true
}

if [ "$PLOT_ONLY" -eq 0 ]; then
  run_set fixed    discard
  run_set buffered buffered
fi

# ── Plot ────────────────────────────────────────────────────────────────
EMPTY="$(mktemp -d)"; trap 'rm -rf "$EMPTY"' EXIT
plot() {                  # $1 results-dir  $2 output basename
  local R="../recording/bench/results/$1"
  ( cd plot && python3 plot_per_compare.py --csv-dir "$EMPTY" --paper --no-windows \
      --xlim 0 2 --figsize 7.5 5 \
      --overlay "802.15.4 replay=$R/per_replay_z2z.csv" \
      --overlay "802.11ah replay=$R/per_replay_h2h.csv" \
      --overlay "cross-PHY replay=$R/per_replay_cross.csv" \
      --overlay-shift "802.15.4 replay=$SHIFT_Z" \
      --overlay-shift "802.11ah replay=$SHIFT_H" \
      --overlay-shift "cross-PHY replay=$SHIFT_X" \
      --overlay-pool 'cross-PHY replay' --overlay-halve 'cross-PHY replay' \
      --out "$2" ) 2>&1 | grep -E 'shifted|halved|wrote'
}
echo; echo "########## figures ##########"
plot fixed    per_soft.png
plot buffered per_soft_buffered.png
# Both policies on one axes: colour = PHY, solid = discard, dashed = buffered.
( cd plot && python3 plot_swap_policy.py --out per_swap_policy.svg ) 2>&1 | grep wrote

# ── Summary, including the validity check ───────────────────────────────
python3 - "$SHIFT_Z" "$SHIFT_H" "$SHIFT_X" <<'PY'
import csv, sys, os
sz, sh, sx = (float(a) for a in sys.argv[1:4])
print("\n########## summary ##########")
bad = 0
for d in ("fixed", "buffered"):
    print(f"\n{d}:")
    for f, name, shift in (("z2z","802.15.4",sz), ("h2h","802.11ah",sh), ("cross","cross-PHY",sx)):
        p=f"recording/bench/results/{d}/per_replay_{f}.csv"
        if not os.path.exists(p): print(f"  {name:<10} MISSING"); continue
        rows=list(csv.DictReader(open(p)))
        n=sum(int(r['swaps']) for r in rows)
        sw=sum(float(r['avg_swap_ms'])*int(r['swaps']) for r in rows)/n
        ov=sum(int(r['overruns']) for r in rows)
        # knee = slowest IFS whose PER first exceeds 1%, with the shift applied
        knee=None
        for r in sorted(rows, key=lambda r: -float(r['ifs_ms'])):
            rx=int(r['rx_H'])+int(r['rx_Z']); s=int(r['sent_H'])+int(r['sent_Z'])
            if 100*(1-rx/s) > 1.0: knee=float(r['ifs_ms'])+shift; break
        bad += ov
        print(f"  {name:<10} swap {sw:6.3f} ms   knee "
              f"{('%.3f ms'%knee) if knee else '   none  '}   overruns {ov}")
if bad:
    print(f"\n  *** {bad} OVERRUN SAMPLES: the flowgraph fell behind 4 MSps real time. ***")
    print("  *** Those points measure the machine, not the swap. Re-run on a quiet ***")
    print("  *** host after perf_unlock.sh, or lower SWEEP_FRAMES.                  ***")
else:
    print("\n  no overruns - every point is attributable to the swap")
PY
echo; echo "figures: plot/per_soft.png (discard), plot/per_soft_buffered.png (buffered),"
echo "         plot/per_swap_policy.svg (both, colour = PHY, dashed = buffered)"
