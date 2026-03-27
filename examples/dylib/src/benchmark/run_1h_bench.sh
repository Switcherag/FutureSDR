#!/bin/bash
#
# run_1h_bench.sh — ~1 hour combined benchmark (FFT + Swap)
#
# FFT:  500 loops × 72 configs  ≈ 25 min
# Swap:  75 loops × 120 configs ≈ 35 min
#
# Usage: ./run_1h_bench.sh [CORES]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CORES="${1:-4}"

START=$(date +%s)

echo "============================================"
echo "  1-HOUR BENCHMARK SUITE"
echo "  Started: $(date)"
echo "============================================"
echo ""

echo ">>> Phase 1/2: FFT sweep (500 loops) <<<"
"$SCRIPT_DIR/run_fft_sweep.sh" "$CORES" 500

MID=$(date +%s)
echo ""
echo "  FFT done in $(( (MID - START) / 60 ))m $(( (MID - START) % 60 ))s"
echo ""

echo ">>> Phase 2/2: Swap sweep (75 loops) <<<"
"$SCRIPT_DIR/run_swap_sweep.sh" "$CORES" 75

END=$(date +%s)
TOTAL=$(( END - START ))

echo ""
echo "============================================"
echo "  COMPLETE"
echo "  FFT:   $(( (MID - START) / 60 ))m $(( (MID - START) % 60 ))s"
echo "  Swap:  $(( (END - MID) / 60 ))m $(( (END - MID) % 60 ))s"
echo "  Total: $(( TOTAL / 60 ))m $(( TOTAL % 60 ))s"
echo "  Finished: $(date)"
echo "============================================"
