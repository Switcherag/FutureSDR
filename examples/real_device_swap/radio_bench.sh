#!/usr/bin/env bash
# The swap benchmark over the air: the bladeRF receiving real frames, the
# receiver replaced after every frame, six ways. For each run the script
# says which transmitter to start, waits for Enter, receives until Enter
# again (when the transmitter is done), then goes on to the next.
#
#   ./radio_bench.sh
#
# The transmitters (yours): ZigBee frames, HaLow frames, and ZigBee and
# HaLow alternating; each sweeping the IFS (e.g. 1000 frames per spacing,
# 6 ms to 0 every 0.01 ms). ZigBee frames of the dyn branch's multizig
# firmware carry their step and IFS in a stamp, which the analysis uses.
#
# Settings (environment):
#   OUT     results directory      (results/radio-<host>-<date>)
#   CPUS    --cpus                 (1,2,3 on a Pi; "auto" on a laptop)
#   EXTRA   more options, e.g. "--keep-awake", "--gain-db 20",
#           "--sample-rate 4e6 --decim 1"
#   ONLY    the runs, e.g. "zz sz" (all by default, in the order below)
#   FRAMES_PER_STEP, IFS_START, IFS_STEP   the transmitters' sweep, for the
#           analysis (1000, 6, 0.01)
#   PAUSE_MS  the HaLow transmitter's pause between spacings, which the
#           analysis cuts the HaLow runs at (500; 0: none, count sequence
#           numbers instead). Must be much longer than the largest IFS.
set -euo pipefail
cd "$(dirname "$0")"

OUT=${OUT:-results/radio-$(hostname)-$(date +%Y%m%d-%H%M)}
CPUS=${CPUS:-1,2,3}
EXTRA=${EXTRA:-}
ONLY=${ONLY:-zz ss gg gd 11 sz}
FRAMES_PER_STEP=${FRAMES_PER_STEP:-1000}
IFS_START=${IFS_START:-6}
IFS_STEP=${IFS_STEP:-0.01}
PAUSE_MS=${PAUSE_MS:-500}

# key: receivers (flows/), transmitter to run, what the run is
declare -A PAIR=(
    [zz]=zigbee.toml,zigbee.toml
    [ss]=wlan_simple.toml,wlan_simple.toml
    [gg]=wlan_granular.toml,wlan_granular.toml
    [gd]=wlan_granular_hard.toml,wlan_granular_viterbi.toml
    [11]=wlan_single.toml,wlan_single.toml
    [sz]=wlan_simple.toml,zigbee.toml
)
declare -A TX=(
    [zz]="ZigBee (2.425 GHz)"
    [ss]="HaLow (919 MHz)"
    [gg]="HaLow (919 MHz)"
    [gd]="HaLow (919 MHz)"
    [11]="HaLow (919 MHz)"
    [sz]="ZigBee and HaLow alternating (each swap retunes)"
)
declare -A WHAT=(
    [zz]="ZigBee receiver replaced by itself"
    [ss]="HaLow receiver (4 blocks) replaced by itself"
    [gg]="HaLow receiver (13 blocks) replaced by itself"
    [gd]="HaLow receiver (13 blocks), its decoder alone replaced (inverse <-> Viterbi)"
    [11]="HaLow receiver in one block replaced by itself"
    [sz]="HaLow (4 blocks) <-> ZigBee receiver, quick-tune retune at each swap"
)

mkdir -p "$OUT"
{
    model=$(tr -d '\0' </proc/device-tree/model 2>/dev/null || grep -m1 'model name' /proc/cpuinfo | cut -d: -f2)
    echo "$(hostname):$model, $(nproc) CPUs; bladeRF; --cpus $CPUS $EXTRA; $(git rev-parse --short HEAD)"
    echo "sweep: $FRAMES_PER_STEP frames per spacing, from $IFS_START ms every $IFS_STEP ms, HaLow pausing $PAUSE_MS ms between spacings"
    echo
    uname -a
    for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
        echo "$g: $(cat "$g")"
    done
    command -v vcgencmd >/dev/null && vcgencmd measure_temp && vcgencmd get_throttled
} >"$OUT/system.txt"

state() {
    if command -v vcgencmd >/dev/null; then
        echo "$(vcgencmd measure_temp) $(vcgencmd get_throttled)"
    fi
}

cargo build --release
echo
echo "== the radio: quick-tune profiles"
FUTURESDR_LOG_LEVEL=warn cargo run -q --release -- --source bladerf --register-only \
    --swap wlan_simple.toml,zigbee.toml $EXTRA | tee "$OUT/radio.txt"

for key in $ONLY; do
    echo
    echo "== $key: ${WHAT[$key]}"
    echo "   receivers: ${PAIR[$key]}"
    echo "   transmitter: ${TX[$key]}"
    read -r -p "   Press Enter to start the receiver (transmitter still idle)... "
    echo "   When it prints 'receiving; press Enter to stop', start the transmitter's sweep;"
    echo "   press Enter once the sweep is over."
    echo "$key start: $(date +%T) $(state)" >>"$OUT/system.txt"
    FUTURESDR_LOG_LEVEL=warn cargo run -q --release -- --source bladerf \
        --cpus "$CPUS" $EXTRA --swap "${PAIR[$key]}" --until-enter \
        --duration 1e9 --rx-timeout-ms 80000 --csv "$OUT/$key.csv" \
        2>&1 | tee "$OUT/$key.log"
    echo "$key end: $(date +%T) $(state)" >>"$OUT/system.txt"
done

python3 figures/plot_radio.py "$OUT" --frames-per-step "$FRAMES_PER_STEP" \
    --ifs-start "$IFS_START" --ifs-step "$IFS_STEP" --pause-ms "$PAUSE_MS"
