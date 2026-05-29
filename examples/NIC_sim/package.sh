#!/usr/bin/env bash
# Build NIC_sim and assemble a RELOCATABLE bundle under examples/NIC_sim/dist/.
#
# The bundle is fully self-contained: the binary, libfuturesdr.so, every plugin
# .so, the Rust libstd, SoapySDR (if linked), the flow TOMLs and the pcap files
# all sit in one directory. You can move/copy that directory anywhere on a
# compatible machine (same CPU arch + glibc) and run it:
#
#   cd dist && ./nic_sim
#
# How relocation works: the binary's RPATH is `$ORIGIN` (set by
# plugin_build.rs, as DT_RPATH via --disable-new-dtags), so the loader resolves
# libfuturesdr.so / libstd / libSoapySDR from the binary's own directory — and
# DT_RPATH of the main executable also covers the dlopen'd plugins' dependency
# on libfuturesdr.so. The flow TOMLs are rewritten to load the pcaps by a
# bundle-relative path, so nothing points back at the source tree.
#
# pcap source dir is overridable:  PCAP_DIR=/path/to/pcaps ./package.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
ROOT_DIR="$(cd "$HERE/../.." && pwd)"
PCAP_DIR="${PCAP_DIR:-/home/alakhdar}"

# 1. Build the ecosystem (forwards --clean if given).
"$HERE/build.sh" "$@" || { echo "package: build step failed"; exit 1; }

TARGET="$ROOT_DIR/target/release"
DIST="$HERE/dist"
rm -rf "$DIST"
mkdir -p "$DIST/flows"

# 2. Binary + libfuturesdr + the exact plugins this example loads.
cp "$TARGET/nic_sim" "$DIST/"
cp "$TARGET/libfuturesdr.so" "$DIST/"
for p in pcap_source blob_to_lp_stream lp_stream_to_blob tap_nic blob_to_udp; do
  cp "$TARGET/lib${p}_plugin.so" "$DIST/" \
    || { echo "package: missing lib${p}_plugin.so — run build.sh first"; exit 1; }
done

# 3. Bundle the non-distro Rust/3rd-party libs so the binary runs even where the
#    toolchain / SoapySDR aren't installed ($ORIGIN is searched first).
copy_dep() { # $1 = ldd match pattern
  local path
  path="$(ldd "$TARGET/nic_sim" 2>/dev/null | awk -v p="$1" '$0 ~ p {print $3; exit}')"
  if [ -n "$path" ] && [ -f "$path" ]; then
    cp "$path" "$DIST/"
    echo "  bundled $(basename "$path")"
  fi
}
copy_dep "libstd-"
copy_dep "libSoapySDR"

# 4. Copy pcaps next to the binary and rewrite the flow configs to load them by
#    a bundle-relative path (resolved from CWD when you `cd dist`).
cp "$PCAP_DIR/halow_tap.pcap" "$DIST/" \
  || { echo "package: $PCAP_DIR/halow_tap.pcap not found (set PCAP_DIR=...)"; exit 1; }
cp "$PCAP_DIR/zigbee_tap.pcap" "$DIST/" \
  || { echo "package: $PCAP_DIR/zigbee_tap.pcap not found (set PCAP_DIR=...)"; exit 1; }
for f in "$HERE"/flows/*.toml; do
  sed -e "s#$PCAP_DIR/halow_tap.pcap#halow_tap.pcap#" \
      -e "s#$PCAP_DIR/zigbee_tap.pcap#zigbee_tap.pcap#" \
      "$f" > "$DIST/flows/$(basename "$f")"
done

echo ""
echo "Bundle ready: $DIST"
echo "Contents:"
( cd "$DIST" && ls -1 ; echo "  flows/:"; ls -1 flows | sed 's/^/    /' )
echo ""
echo "TAP setup (once, needs CAP_NET_ADMIN):"
echo "  sudo ip tuntap add dev sdrtap0 mode tap user \"\$USER\""
echo "  sudo ip link set sdrtap0 address de:ad:be:ef:00:01 && sudo ip link set sdrtap0 up"
echo ""
echo "Run from anywhere after moving the folder:"
echo "  cd '$DIST' && ./nic_sim"
