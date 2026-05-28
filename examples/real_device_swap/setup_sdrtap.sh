#!/usr/bin/env bash
# Create the persistent TAP iface that tap_nic_plugin attaches to.
#
# Run once per boot:
#     ./setup_sdrtap.sh
#
# Idempotent: if the iface already exists, it just verifies + prints state.
# After running, you can launch zigbee_swap (and friends) as your normal
# user — TUNSETIFF passes because the iface is owned by $USER.
#
# Tear-down (optional, when you want a clean slate):
#     sudo ip link del sdrtap0

set -euo pipefail

IFACE="${IFACE:-sdrtap0}"
MAC="${MAC:-de:ad:be:ef:00:01}"
OWNER="${OWNER:-$USER}"

# Keep the config aligned with examples/real_device_swap/flows/network_tail.toml's
# tap_nic block. If you change either, change both.
EXPECTED_CFG="${IFACE}:${MAC}"
TAIL_TOML="$(dirname "$0")/flows/network_tail.toml"
if [[ -f "$TAIL_TOML" ]] && ! grep -qF "config = \"${EXPECTED_CFG}\"" "$TAIL_TOML"; then
    echo "warning: ${IFACE}:${MAC} does not match the tap_nic config in $TAIL_TOML" >&2
    echo "         (proceeding anyway — adjust IFACE/MAC env vars or edit the TOML)" >&2
fi

if ip link show "$IFACE" >/dev/null 2>&1; then
    echo "iface ${IFACE} already exists — verifying state"
else
    echo "creating ${IFACE} (mode tap, owner=${OWNER})"
    sudo ip tuntap add dev "$IFACE" mode tap user "$OWNER"
fi

current_mac="$(cat "/sys/class/net/${IFACE}/address" 2>/dev/null || echo '')"
if [[ "$current_mac" != "$MAC" ]]; then
    echo "setting MAC ${MAC} (was ${current_mac:-unset})"
    sudo ip link set dev "$IFACE" address "$MAC"
fi

oper_state="$(cat "/sys/class/net/${IFACE}/operstate" 2>/dev/null || echo 'unknown')"
if [[ "$oper_state" != "up" && "$oper_state" != "unknown" ]]; then
    echo "bringing ${IFACE} up (was ${oper_state})"
    sudo ip link set dev "$IFACE" up
fi

echo
echo "── ${IFACE} ready ──"
ip -d link show "$IFACE"
echo
echo "run the binary as your normal user (no sudo):"
echo "    cd $(dirname "$0") && ../../target/release/zigbee_swap"
