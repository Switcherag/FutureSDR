#!/bin/bash
set -e

# ══════════════════════════════════════════════════════════════════
# 30-Minute Random Frequency Change Benchmark
# ══════════════════════════════════════════════════════════════════
#
# Tests 4 methods for frequency change latency on bladeRF 2.0:
#   1. libbladeRF set_frequency    — native C, full PLL recalc
#   2. libbladeRF schedule_retune  — native C, pre-computed quick_tune
#   3. seify/SoapySDR              — Rust, via SoapySDR wrapper
#   4. seify/bladerf1              — Rust, native libbladerf-rs driver
#
# Random frequencies across the full 47 MHz – 6 GHz range.
# ~7.5 minutes per method = ~30 minutes total.
#
# Output:
#   bladerf_bench_c/bladerf_random_freq_bench.csv
#   sdr_random_freq_bench.csv
#   random_freq_bench_comparison.png
# ══════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
C_DIR="$SCRIPT_DIR/bladerf_bench_c"

DURATION_MIN=${1:-7.5}

echo "════════════════════════════════════════════════════════════"
echo "  Random Frequency Change Benchmark — bladeRF 2.0"
echo "  Duration per method: ${DURATION_MIN} min (~$(echo "$DURATION_MIN * 4" | bc) min total)"
echo "════════════════════════════════════════════════════════════"
echo ""

# ── Step 1: Build C benchmarks ──────────────────────────────────
echo "Step 1: Building C benchmarks..."
cd "$C_DIR"
make bladerf_random_freq_bench 2>&1 | tail -3
echo ""

# ── Step 2: Build Rust benchmarks ───────────────────────────────
echo "Step 2: Building Rust benchmarks..."
cd "$ROOT_DIR"
cargo build --release -p sdr_tuning --bin sdr-random-freq-bench 2>&1 | tail -5
echo ""

# ── Step 3: Run C benchmark (set_frequency + quick_tune) ───────
echo "════════════════════════════════════════════════════════════"
echo "Step 3: C benchmark — set_frequency + schedule_retune"
echo "════════════════════════════════════════════════════════════"
cd "$C_DIR"
LD_LIBRARY_PATH="/usr/local/lib64:${LD_LIBRARY_PATH}" \
    ./bladerf_random_freq_bench "$DURATION_MIN"
echo ""

# ── Step 4: Run Rust/Soapy benchmark ───────────────────────────
echo "════════════════════════════════════════════════════════════"
echo "Step 4: Rust benchmark — seify/SoapySDR"
echo "════════════════════════════════════════════════════════════"
cd "$SCRIPT_DIR"
"$ROOT_DIR/target/release/sdr-random-freq-bench" \
    --duration "$DURATION_MIN" \
    --mode soapy
echo ""

# ── Step 5: Run Rust/bladerf1 benchmark ─────────────────────────
echo "════════════════════════════════════════════════════════════"
echo "Step 5: Rust benchmark — seify/bladerf1"
echo "════════════════════════════════════════════════════════════"
cd "$SCRIPT_DIR"
"$ROOT_DIR/target/release/sdr-random-freq-bench" \
    --duration "$DURATION_MIN" \
    --mode bladerf1
echo ""

# ── Step 6: Merge CSVs ──────────────────────────────────────────
echo "════════════════════════════════════════════════════════════"
echo "Step 6: Merge results"
echo "════════════════════════════════════════════════════════════"

# Append Rust results to a combined CSV if both exist
# The plot script reads from both files directly, so no merge needed.
echo "  C results:    $C_DIR/bladerf_random_freq_bench.csv"
echo "  Rust results: $SCRIPT_DIR/sdr_random_freq_bench.csv"

# ── Step 7: Plot ────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════════════"
echo "Step 7: Generating plots"
echo "════════════════════════════════════════════════════════════"
cd "$SCRIPT_DIR"
python3 plot_random_freq_bench.py

echo ""
echo "════════════════════════════════════════════════════════════"
echo "  DONE — see random_freq_bench_comparison.png"
echo "════════════════════════════════════════════════════════════"
