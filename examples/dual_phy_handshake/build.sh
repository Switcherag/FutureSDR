#!/usr/bin/env bash
# Release build of the dual_phy_handshake example + the plugins it loads.
#
# Built in ONE cargo invocation so futuresdr is compiled once and the example
# binaries, libfuturesdr.so and every plugin land in target/release/ with a
# matching ABI (see real_device_swap/build.sh for the full rationale).
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/../.." && pwd)"
cd "$ROOT_DIR"

ECOSYSTEM=(
  dual-phy-handshake
  # RX head
  seify_source_plugin fir_resampler_plugin
  # 802.15.4 listen PHY
  zigbee_demod_plugin clock_recovery_mm_plugin zigbee_decoder_plugin zigbee_mac_plugin null_sink_plugin
  # 802.11ah listen PHY (the `ack` binary can listen on this band too)
  wlan_ah_v2_stf_detector_plugin wlan_ah_v2_cfo_corrector_plugin wlan_ah_v2_sto_corrector_plugin
  wlan_ah_v2_channel_estimator_plugin wlan_ah_v2_sig_decoder_plugin wlan_ah_v2_data_demod_plugin
  wlan_ah_decoder_plugin
  # RFTAP egress (both listen PHYs send decoded frames to UDP for Wireshark)
  blob_to_udp_plugin
  # TX sink radio
  seify_sink_plugin
)

PKG_ARGS=()
for p in "${ECOSYSTEM[@]}"; do PKG_ARGS+=(-p "$p"); done

if [ "${1:-}" = "--clean" ]; then
  cargo clean --release 2>&1 | tail -3
fi
cargo build --release "${PKG_ARGS[@]}" || { echo "build FAILED"; exit 1; }

echo ""
echo "Built. Next:"
echo "  cd examples/dual_phy_handshake"
echo "  ../../target/release/gen_waveforms          # precompute ACK .cf32 files"
echo "  ../../target/release/dual_phy_handshake     # run the handshake"
