#!/usr/bin/env bash
# Put the CPU in its fastest sustained state before benchmarking, and report
# what actually took effect. Needs root for the writes; without it, it only
# reports (and the benchmark still runs, just slower and noisier).
#
# This matters more than it sounds. On the development machine the governor was
# already `performance` yet `scaling_max_freq` sat pinned at the 2.10 GHz base
# clock against a 5.10 GHz capability -- every swap measured 1.67x too slow, and
# it looked like a code regression until the cap was found. Always run this (or
# at least its report) before trusting a number.
set -u

say() { printf '  %-34s %s\n' "$1" "$2"; }
w() { # write $2 to every path matching $1, ignore failures
  local n=0
  for f in $1; do [ -w "$f" ] && { echo "$2" > "$f" 2>/dev/null && n=$((n+1)); }; done
  echo "$n"
}

echo "=== CPU performance unlock ==="
[ "$(id -u)" -eq 0 ] || echo "  NOTE: not root — reporting only, no changes applied"

# 1. Governor -> performance
n=$(w "/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor" performance)
say "governor set on" "$n core(s)"

# 2. Raise scaling_max_freq to the hardware maximum. THIS is the one that bit us.
for c in /sys/devices/system/cpu/cpu*/cpufreq; do
  [ -r "$c/cpuinfo_max_freq" ] || continue
  [ -w "$c/scaling_max_freq" ] && cat "$c/cpuinfo_max_freq" > "$c/scaling_max_freq" 2>/dev/null
done
say "scaling_max raised" "attempted"

# 3. Energy-performance preference, where the driver exposes it
n=$(w "/sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference" performance)
say "EPP set on" "$n core(s)"

# 4. Intel turbo, if present (no-op on a Pi)
[ -w /sys/devices/system/cpu/intel_pstate/no_turbo ] && echo 0 > /sys/devices/system/cpu/intel_pstate/no_turbo
[ -w /sys/devices/system/cpu/cpufreq/boost ] && echo 1 > /sys/devices/system/cpu/cpufreq/boost

# 5. Daemons that re-clamp behind your back.
for s in thermald power-profiles-daemon tlp auto-cpufreq ondemand; do
  systemctl is-active "$s" >/dev/null 2>&1 && say "WARNING: active daemon" "$s (may re-clamp the cap)"
done

echo
echo "=== state ==="
say "governor" "$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a)"
for f in scaling_max_freq cpuinfo_max_freq; do
  v=$(cat "/sys/devices/system/cpu/cpu0/cpufreq/$f" 2>/dev/null) || continue
  say "$f" "$(awk -v v="$v" 'BEGIN{printf "%.2f GHz", v/1e6}')"
done
smax=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq 2>/dev/null || echo 0)
hmax=$(cat /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq 2>/dev/null || echo 0)
if [ "$smax" -gt 0 ] && [ "$hmax" -gt 0 ] && [ "$smax" -lt "$hmax" ]; then
  echo
  echo "  *** STILL CAPPED: scaling_max is below the hardware maximum. ***"
  echo "  *** Benchmark numbers will be slow and NOT comparable to an uncapped run. ***"
  echo "  On a Raspberry Pi this usually means firmware: set in /boot/firmware/config.txt"
  echo "      arm_freq=2400        (or your board's rated clock)"
  echo "      force_turbo=1"
  echo "  then reboot. Check for throttling with: vcgencmd get_throttled  (0x0 = clean)"
fi
command -v vcgencmd >/dev/null 2>&1 && say "throttled flags" "$(vcgencmd get_throttled 2>/dev/null || echo n/a)"
say "cores" "$(nproc)"
say "load average" "$(cut -d' ' -f1-3 /proc/loadavg)"
