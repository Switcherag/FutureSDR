#!/usr/bin/env bash
# Release build of the NIC_sim plugin ecosystem.
#
# Everything is built in ONE cargo invocation so futuresdr is compiled exactly
# once, with one feature unification (the ABI contract in the root
# [workspace.dependencies]). The example binary, libfuturesdr.so and every
# plugin land in target/release/ and find each other at runtime via the
# $ORIGIN rpath set by plugin_build.rs.
set -uo pipefail

# Run from the workspace root so .cargo/config.toml (-C prefer-dynamic) and the
# workspace Cargo.toml are picked up regardless of where this is invoked.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/../.." && pwd)"
cd "$ROOT_DIR"

ECOSYSTEM=(
  nic-sim-example
  pcap_source_plugin
  blob_to_lp_stream_plugin
  lp_stream_to_blob_plugin
  tap_nic_plugin
  blob_to_udp_plugin
)

PKG_ARGS=()
for p in "${ECOSYSTEM[@]}"; do PKG_ARGS+=(-p "$p"); done

# ABI STABILITY RULE: always build the SAME package set, in ONE invocation.
# cargo's per-unit metadata hash (baked into every Rust-ABI symbol) depends on
# the shape of the resolve, i.e. which packages are selected — NOT just on their
# features. Building a subset (e.g. `cargo build -p one_plugin`) re-resolves and
# gives futuresdr + its deps DIFFERENT metadata, which silently breaks every
# already-built plugin. So never `-p` a subset: edit a plugin, re-run THIS
# script. cargo's incremental engine then recompiles only the changed plugin and
# reuses the cached libfuturesdr.so → fast partial rebuild, stable ABI.
#
# Pass --clean for a from-scratch baseline (new toolchain / futuresdr changes).
if [ "${1:-}" = "--clean" ]; then
  cargo clean --release 2>&1 | tail -3
fi
cargo build --release "${PKG_ARGS[@]}" || { echo "build FAILED"; exit 1; }

# ── ABI guard ────────────────────────────────────────────────────
# Catch the two silent failure modes:
#   1. a plugin statically embedded libfuturesdr (prefer-dynamic not applied)
#   2. a plugin imports futuresdr symbols that don't exist in the deployed
#      libfuturesdr.so (feature/metadata drift -> would crash on load/call)
TARGET="target/release"
LIB="$TARGET/libfuturesdr.so"
echo ""
echo "=== ABI guard ($LIB) ==="
[ -f "$LIB" ] || { echo "FAIL: $LIB not found"; exit 1; }

LIBSYMS="$(mktemp)"
nm -D --defined-only "$LIB" 2>/dev/null | awk '{print $NF}' | LC_ALL=C sort -u > "$LIBSYMS"
echo "  lib metadata: $(nm -D "$LIB" 2>/dev/null | grep -oE 'rust_metadata_futuresdr_[0-9a-f]+')"

fail=0
for so in "$TARGET"/lib*_plugin.so; do
  [ -e "$so" ] || continue
  name="$(basename "$so")"

  if ! objdump -p "$so" 2>/dev/null | grep -q 'NEEDED .*libfuturesdr.so'; then
    echo "  EMBED  $name  (does not dynamically link libfuturesdr.so)"
    fail=1
    continue
  fi

  missing="$(nm -D -u "$so" 2>/dev/null | awk '{print $NF}' | grep -F futuresdr \
            | LC_ALL=C sort -u | LC_ALL=C comm -23 - "$LIBSYMS")"
  if [ -n "$missing" ]; then
    n="$(printf '%s\n' "$missing" | grep -c .)"
    echo "  ABIBAD $name  ($n futuresdr symbols not in libfuturesdr.so), e.g.:"
    printf '%s\n' "$missing" | head -2 | sed 's/^/           /'
    fail=1
    continue
  fi

  printf "  OK     %s  (%s)\n" "$name" "$(du -h "$so" | cut -f1)"
done

rm -f "$LIBSYMS"
if [ "$fail" -ne 0 ]; then
  echo "ABI guard FAILED"
  exit 1
fi
echo "ABI guard PASSED — all plugins dynamic and symbol-compatible."
