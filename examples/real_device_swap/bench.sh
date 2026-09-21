#!/usr/bin/env bash
# The six swaps against the software IFS, 0 to 6 ms every 0.01 ms (as the
# transmitters of radio_bench.sh sweep it), no
# radio: the receiver replaced after every frame it posts. Writes a CSV per
# swap, system.txt, bench.png and summary.md into $OUT.
#
#   ./bench.sh                                   # Raspberry Pi 5
#   CPUS=auto EXTRA=--keep-awake ./bench.sh      # the laptop
#
# Settings (environment):
#   OUT     results directory      (results/<host>-<date>)
#   FRAMES  frames per spacing     (400: 200 of each receiver)
#   CPUS    --cpus                 (1,2,3: runtime on CPUs 1-3, the replay
#                                   on CPU 0)
#   EXTRA   more options, e.g. "--keep-awake" or "--rt-priority 50"
#   ONLY    the swaps to run, e.g. "zz gd" (all by default)
#   STEP    spacing step in ms     (0.01)
#   MAX     largest spacing in ms  (6)
#
# At 400 frames a spacing takes 0.6 to 0.9 s plus 0.4 s per ms of IFS: about
# 2 s on average from 0 to 6 ms, so 17 to 21 min per swap and about 1 h 50
# for the six on a laptop, longer on a Pi.
set -euo pipefail
cd "$(dirname "$0")"

OUT=${OUT:-results/$(hostname)-$(date +%Y%m%d-%H%M)}
FRAMES=${FRAMES:-400}
CPUS=${CPUS:-1,2,3}
EXTRA=${EXTRA:-}
ONLY=${ONLY:-zz ss sz gg gd 11}
STEP=${STEP:-0.01}
MAX=${MAX:-6}

# Largest first, as in the dyn branch's sweeps.
IFS_LIST=$(python3 -c "
n = round($MAX / $STEP)
print(','.join(f'{k * $STEP:.4g}' for k in range(n, -1, -1)))")

declare -A PAIRS=(
    [zz]=zigbee.toml,zigbee.toml
    [ss]=wlan_simple.toml,wlan_simple.toml
    [sz]=wlan_simple.toml,zigbee.toml
    [gg]=wlan_granular.toml,wlan_granular.toml
    [gd]=wlan_granular_hard.toml,wlan_granular_viterbi.toml
    [11]=wlan_single.toml,wlan_single.toml
)

mkdir -p "$OUT"
{
    model=$(tr -d '\0' </proc/device-tree/model 2>/dev/null || grep -m1 'model name' /proc/cpuinfo | cut -d: -f2)
    echo "$(hostname):$model, $(nproc) CPUs; --cpus $CPUS $EXTRA; $(git rev-parse --short HEAD)"
    echo
    uname -a
    for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
        echo "$g: $(cat "$g")"
    done
    command -v vcgencmd >/dev/null && vcgencmd measure_temp && vcgencmd get_throttled
} >"$OUT/system.txt"

cargo build --release

state() {
    if command -v vcgencmd >/dev/null; then
        echo "$(vcgencmd measure_temp) $(vcgencmd get_throttled)"
    fi
}

for key in $ONLY; do
    pair=${PAIRS[$key]}
    echo "== $key: $pair  $(state)"
    FUTURESDR_LOG_LEVEL=warn cargo run -q --release -- --source replay \
        --cpus "$CPUS" $EXTRA --retune-us 0 --swap "$pair" \
        --ifs "$IFS_LIST" --frames-per-step "$FRAMES" --csv "$OUT/$key.csv" \
        2>&1 | tee "$OUT/$key.log" | grep -E "runtime threads|source thread|→"
    echo "   done  $(state)"
    echo "$key after: $(state)" >>"$OUT/system.txt"
done

python3 figures/plot_bench.py "$OUT"
