#!/bin/bash
#
# run_isolated_bench.sh — Reserve N performance cores and run a benchmark in isolation.
#
# Usage:  ./run_isolated_bench.sh N COMMAND [ARGS...]
#
# What it does:
#   1. Picks the first N "performance" cores (highest cpu_capacity, or first N if homogeneous)
#   2. Sets the 'performance' governor on those cores, disables turbo boost and deep C-states
#   3. Migrates all system processes to the *remaining* cores
#   4. Runs COMMAND on the reserved cores (via systemd-run) so it has them exclusively
#   5. Reverts everything on exit (trap)
#
# Requires: root (sudo), systemd, cpufreq sysfs
#

set -euo pipefail

# ── Args ─────────────────────────────────────────────────────────────────────
if [[ $# -lt 2 ]]; then
    echo "Usage: $0 [-r REPEAT] N COMMAND [ARGS...]"
    echo "  -r REPEAT — run COMMAND this many times (default: 1)"
    echo "  N         — number of performance cores to reserve"
    echo "  COMMAND   — benchmark executable (followed by its arguments)"
    exit 1
fi

REPEAT=1
if [[ "$1" == "-r" ]]; then
    REPEAT="$2"; shift 2
fi

NUM_CORES="$1"; shift
BENCH_CMD=("$@")

TOTAL_CPUS=$(nproc --all)
if (( NUM_CORES <= 0 || NUM_CORES >= TOTAL_CPUS )); then
    echo "Error: N must be between 1 and $((TOTAL_CPUS - 1)) (need at least 1 core for the system)"
    exit 1
fi

# ── Detect performance cores ─────────────────────────────────────────────────
# Read (cpu_id, capacity) pairs.  If cpu_capacity is not available (older
# kernels / non-hybrid CPUs), every core gets the same synthetic capacity so
# the sort is stable and we just pick the first N by CPU id.

declare -a ALL_CPUS_SORTED=()

if [[ -f /sys/devices/system/cpu/cpu0/cpu_capacity ]]; then
    # Sort by capacity descending, then by CPU id ascending (stable pick)
    while IFS= read -r line; do
        ALL_CPUS_SORTED+=("$line")
    done < <(
        for cpu_dir in /sys/devices/system/cpu/cpu[0-9]*; do
            cpu_id="${cpu_dir##*cpu}"
            cap=$(cat "$cpu_dir/cpu_capacity" 2>/dev/null || echo 1024)
            echo "$cpu_id $cap"
        done | sort -k2,2rn -k1,1n | awk '{print $1}'
    )
else
    # No capacity info — just use 0..N-1 order
    for (( i=0; i<TOTAL_CPUS; i++ )); do
        ALL_CPUS_SORTED+=("$i")
    done
fi

# Split into reserved (benchmark) and system cores
RESERVED_CPUS=("${ALL_CPUS_SORTED[@]:0:$NUM_CORES}")
SYSTEM_CPUS=("${ALL_CPUS_SORTED[@]:$NUM_CORES}")

# Helper: join array with a separator
join_by() { local IFS="$1"; shift; echo "$*"; }

RESERVED_LIST=$(join_by ',' "${RESERVED_CPUS[@]}")
SYSTEM_LIST=$(join_by ',' "${SYSTEM_CPUS[@]}")

echo "==> Total CPUs: $TOTAL_CPUS"
echo "==> Reserved cores (benchmark): $RESERVED_LIST"
echo "==> System cores (everything else): $SYSTEM_LIST"

# ── State we need to restore ─────────────────────────────────────────────────
declare -A ORIG_GOVERNOR=()
declare -A ORIG_MIN_FREQ=()
declare -A ORIG_MAX_FREQ=()
ORIG_TURBO=""
TURBO_FILE=""

cleanup() {
    echo ""
    echo "==> Cleaning up..."

    # Restore governors and frequencies
    for cpu_id in "${RESERVED_CPUS[@]}"; do
        local cpu_dir="/sys/devices/system/cpu/cpu${cpu_id}"
        if [[ -n "${ORIG_GOVERNOR[$cpu_id]:-}" ]]; then
            echo "${ORIG_GOVERNOR[$cpu_id]}" | sudo tee "$cpu_dir/cpufreq/scaling_governor" >/dev/null 2>&1 || true
        fi
        if [[ -n "${ORIG_MIN_FREQ[$cpu_id]:-}" ]]; then
            echo "${ORIG_MIN_FREQ[$cpu_id]}" | sudo tee "$cpu_dir/cpufreq/scaling_min_freq" >/dev/null 2>&1 || true
        fi
        if [[ -n "${ORIG_MAX_FREQ[$cpu_id]:-}" ]]; then
            echo "${ORIG_MAX_FREQ[$cpu_id]}" | sudo tee "$cpu_dir/cpufreq/scaling_max_freq" >/dev/null 2>&1 || true
        fi
    done

    # Restore turbo boost
    if [[ -n "$TURBO_FILE" && -n "$ORIG_TURBO" ]]; then
        echo "$ORIG_TURBO" | sudo tee "$TURBO_FILE" >/dev/null 2>&1 || true
    fi

    # Re-enable C-states on reserved cores
    for cpu_id in "${RESERVED_CPUS[@]}"; do
        for state_file in /sys/devices/system/cpu/cpu${cpu_id}/cpuidle/state*/disable; do
            [[ -f "$state_file" ]] && echo 0 | sudo tee "$state_file" >/dev/null 2>&1 || true
        done
    done

    # Revert process migration — allow all CPUs
    ALL_RANGE="0-$((TOTAL_CPUS - 1))"
    sudo systemctl set-property --runtime system.slice AllowedCPUs="$ALL_RANGE" 2>/dev/null || true
    sudo systemctl set-property --runtime user.slice   AllowedCPUs="$ALL_RANGE" 2>/dev/null || true
    sudo systemctl set-property --runtime init.scope   AllowedCPUs="$ALL_RANGE" 2>/dev/null || true

    echo "==> Cleanup complete."
}
trap cleanup EXIT

# ── 1. Save & set performance governor + lock max frequency ──────────────────
echo "==> Setting 'performance' governor on reserved cores..."
for cpu_id in "${RESERVED_CPUS[@]}"; do
    cpu_dir="/sys/devices/system/cpu/cpu${cpu_id}"

    if [[ -f "$cpu_dir/cpufreq/scaling_governor" ]]; then
        ORIG_GOVERNOR[$cpu_id]=$(cat "$cpu_dir/cpufreq/scaling_governor")
        echo performance | sudo tee "$cpu_dir/cpufreq/scaling_governor" >/dev/null
    fi

    if [[ -f "$cpu_dir/cpufreq/scaling_min_freq" ]]; then
        ORIG_MIN_FREQ[$cpu_id]=$(cat "$cpu_dir/cpufreq/scaling_min_freq")
    fi
    if [[ -f "$cpu_dir/cpufreq/scaling_max_freq" ]]; then
        ORIG_MAX_FREQ[$cpu_id]=$(cat "$cpu_dir/cpufreq/scaling_max_freq")
        # Lock to max: set min = max so the frequency is pinned
        max_freq=$(cat "$cpu_dir/cpufreq/cpuinfo_max_freq" 2>/dev/null || echo "")
        if [[ -n "$max_freq" ]]; then
            echo "$max_freq" | sudo tee "$cpu_dir/cpufreq/scaling_min_freq" >/dev/null
            echo "$max_freq" | sudo tee "$cpu_dir/cpufreq/scaling_max_freq" >/dev/null
        fi
    fi
done

# ── 2. Disable turbo boost ───────────────────────────────────────────────────
echo "==> Disabling turbo boost..."
if [[ -f /sys/devices/system/cpu/intel_pstate/no_turbo ]]; then
    TURBO_FILE="/sys/devices/system/cpu/intel_pstate/no_turbo"
    ORIG_TURBO=$(cat "$TURBO_FILE")
    echo 1 | sudo tee "$TURBO_FILE" >/dev/null
elif [[ -f /sys/devices/system/cpu/cpufreq/boost ]]; then
    TURBO_FILE="/sys/devices/system/cpu/cpufreq/boost"
    ORIG_TURBO=$(cat "$TURBO_FILE")
    echo 0 | sudo tee "$TURBO_FILE" >/dev/null
else
    echo "    (no turbo boost control found — skipping)"
fi

# ── 3. Disable deep C-states on reserved cores ──────────────────────────────
echo "==> Disabling C-states (except C0) on reserved cores..."
for cpu_id in "${RESERVED_CPUS[@]}"; do
    for state_file in /sys/devices/system/cpu/cpu${cpu_id}/cpuidle/state*/disable; do
        [[ ! -f "$state_file" ]] && continue
        state=$(basename "$(dirname "$state_file")")
        if [[ "$state" != "state0" ]]; then
            echo 1 | sudo tee "$state_file" >/dev/null 2>&1 || true
        fi
    done
done

# ── 4. Migrate all system processes to system cores ──────────────────────────
echo "==> Migrating all system processes to cores: $SYSTEM_LIST"
sudo systemctl set-property --runtime system.slice AllowedCPUs="$SYSTEM_LIST"
sudo systemctl set-property --runtime user.slice   AllowedCPUs="$SYSTEM_LIST"
sudo systemctl set-property --runtime init.scope   AllowedCPUs="$SYSTEM_LIST"

# ── 5. Run the benchmark isolated on reserved cores ──────────────────────────
echo "==> Running benchmark on cores: $RESERVED_LIST"
echo "    Command: ${BENCH_CMD[*]}"
echo "    Repeat: $REPEAT"
echo "────────────────────────────────────────────────────────────"

EXIT_CODE=0
for (( iter=1; iter<=REPEAT; iter++ )); do
    if (( REPEAT > 1 )); then
        echo "==> Iteration $iter/$REPEAT"
    fi
    sudo systemd-run \
        --uid="$(id -u)" \
        --gid="$(id -g)" \
        --slice=bench \
        --wait \
        -P \
        -p AllowedCPUs="$RESERVED_LIST" \
        --working-directory="$(pwd)" \
        -d \
        -- "${BENCH_CMD[@]}"

    rc=$?
    if (( rc != 0 )); then
        echo "==> FAILED at iteration $iter (exit code: $rc)"
        EXIT_CODE=$rc
        break
    fi
done

echo "────────────────────────────────────────────────────────────"
echo "==> Benchmark finished ($REPEAT iterations, exit code: $EXIT_CODE)"

exit $EXIT_CODE
