#!/bin/bash
#
# run_swap_sweep.sh — Swap benchmark: N×FFT → terminate → N×IFFT, static vs dynv2.
#
# Sweeps: block counts × FFT sizes × data lengths × methods
#
# Usage: ./run_swap_sweep.sh [CORES] [LOOPS]
#   CORES — number of performance cores to reserve (default: 4)
#   LOOPS — iterations per configuration (default: 30)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
ISOLATE="$PROJECT_ROOT/perf/run_isolated_bench.sh"
PLUGIN_DIR="$PROJECT_ROOT/shared_libs/release"
OUTPUT="$RESULTS_DIR/swap_sweep.txt"

CORES="${1:-4}"
LOOPS="${2:-30}"

BLOCK_COUNTS=(2 4 8 16 32 64)
FFT_SIZES=(64 256 1024)
N_SAMPLES=(64 4096 262144 16777216)

mkdir -p "$RESULTS_DIR"
> "$OUTPUT"

# Count valid configs (skip when n_samples < fft_size)
TOTAL_CONFIGS=0
for blocks in "${BLOCK_COUNTS[@]}"; do
    for fft_size in "${FFT_SIZES[@]}"; do
        for ns in "${N_SAMPLES[@]}"; do
            if (( ns >= fft_size )); then
                TOTAL_CONFIGS=$(( TOTAL_CONFIGS + 2 ))
            fi
        done
    done
done
CURRENT=0

echo "=== Swap Benchmark Sweep ==="
echo "  Block counts: ${BLOCK_COUNTS[*]}"
echo "  FFT sizes:    ${FFT_SIZES[*]}"
echo "  N_samples:    ${N_SAMPLES[*]}"
echo "  Loops:        $LOOPS"
echo "  Cores:        $CORES"
echo "  Output:       $OUTPUT"
echo "  Total configs: $TOTAL_CONFIGS"
echo ""

for blocks in "${BLOCK_COUNTS[@]}"; do
    for fft_size in "${FFT_SIZES[@]}"; do
        for ns in "${N_SAMPLES[@]}"; do
            # Skip if n_samples < fft_size (would round to 0)
            if (( ns < fft_size )); then
                echo "  SKIP  blocks=$blocks  fft=$fft_size  n_samples=$ns  (n_samples < fft_size)"
                continue
            fi

            for method in swap_static swap_dynv2; do
                CURRENT=$(( CURRENT + 1 ))
                echo "[$CURRENT/$TOTAL_CONFIGS] $method  blocks=$blocks  fft=$fft_size  n_samples=$ns"

                CMD=("$PROJECT_ROOT/target/release/$method"
                     --n-blocks "$blocks"
                     --fft-size "$fft_size"
                     --n-samples "$ns"
                     --loop-count "$LOOPS"
                     --output "$OUTPUT")

                if [[ "$method" == "swap_dynv2" ]]; then
                    CMD+=(--plugin-dir "$PLUGIN_DIR")
                fi

                "$ISOLATE" "$CORES" "${CMD[@]}" 2>/dev/null || {
                    echo "  FAILED (exit $?), skipping..."
                    continue
                }
            done
        done
    done
done

LINES=$(wc -l < "$OUTPUT")
echo ""
echo "=== Sweep complete: $LINES data points in $OUTPUT ==="
