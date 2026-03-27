#!/bin/bash
#
# run_fft_sweep.sh — Full parameter sweep: block counts × FFT sizes, static vs dynv2.
#
# Usage: ./run_fft_sweep.sh [CORES] [LOOPS]
#   CORES — number of performance cores to reserve (default: 4)
#   LOOPS — iterations per configuration (default: 30)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
ISOLATE="$PROJECT_ROOT/perf/run_isolated_bench.sh"
PLUGIN_DIR="$PROJECT_ROOT/shared_libs/release"
OUTPUT="$RESULTS_DIR/fft_sweep.txt"

CORES="${1:-4}"
LOOPS="${2:-30}"

BLOCK_COUNTS=(2 4 8 16 32 64)
FFT_SIZES=(64 128 256 512 1024 2048)

mkdir -p "$RESULTS_DIR"
> "$OUTPUT"

TOTAL_CONFIGS=$(( ${#BLOCK_COUNTS[@]} * ${#FFT_SIZES[@]} * 2 ))
CURRENT=0

echo "=== FFT Benchmark Sweep ==="
echo "  Block counts: ${BLOCK_COUNTS[*]}"
echo "  FFT sizes:    ${FFT_SIZES[*]}"
echo "  Loops:        $LOOPS"
echo "  Cores:        $CORES"
echo "  Output:       $OUTPUT"
echo "  Total configs: $TOTAL_CONFIGS"
echo ""

for blocks in "${BLOCK_COUNTS[@]}"; do
    pairs=$(( blocks / 2 ))
    for fft_size in "${FFT_SIZES[@]}"; do
        n_samples=$(( fft_size * 1000 ))

        for method in fft_static fft_dynv2; do
            CURRENT=$(( CURRENT + 1 ))
            echo "[$CURRENT/$TOTAL_CONFIGS] $method  blocks=$blocks  fft_size=$fft_size  n_samples=$n_samples"

            CMD=("$PROJECT_ROOT/target/release/$method"
                 --fft-pairs "$pairs"
                 --fft-size "$fft_size"
                 --n-samples "$n_samples"
                 --loop-count "$LOOPS"
                 --output "$OUTPUT")

            if [[ "$method" == "fft_dynv2" ]]; then
                CMD+=(--plugin-dir "$PLUGIN_DIR")
            fi

            "$ISOLATE" "$CORES" "${CMD[@]}" 2>/dev/null || {
                echo "  FAILED (exit $?), skipping..."
                continue
            }
        done
    done
done

LINES=$(wc -l < "$OUTPUT")
echo ""
echo "=== Sweep complete: $LINES data points in $OUTPUT ==="
