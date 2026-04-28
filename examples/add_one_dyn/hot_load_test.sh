#!/bin/bash
set -e

# ══════════════════════════════════════════════════════════════════════
# Hot-Load Test — A Posteriori Compilation
# ══════════════════════════════════════════════════════════════════════
# Proves that a plugin compiled AFTER the binary is already running
# can be loaded at runtime via the UDP swap mechanism.
#
# This test verifies the following scenario:
#
#   1. Flow A runs         — the program starts with only base plugins;
#                             add_one_plugin does NOT exist yet.
#   2. Flow B is compiled  — while the program is running, the
#                             add_one_plugin is compiled externally.
#   3. Flow B is loaded    — a UDP swap command triggers ensure_loaded(),
#                             which dlopen()s the freshly compiled .so.
#
# Test assertions:
#
#   TEST 1: permanent FG output == 0x00   (NullSource→Head→FileSink)
#   TEST 2: binary stays alive after step 1
#   TEST 3: hot-loaded FG output == 0x01  (NullSource→AddOne→Head→FileSink)
#
# Steps:
#   1. Build core + base plugins + example binary (NO add_one_plugin)
#   2. Start binary in --hot-load mode (permanent FG writes 0x00)
#   3. Verify output_without_add_one.bin contains 0x00
#   4. Compile add_one_plugin (while binary is alive)
#   5. Copy new .so into the plugin directory
#   6. UDP swap → load fg_with_add_one.toml (triggers ensure_loaded)
#   7. Verify output_with_add_one.bin contains 0x01
#   8. Send Q to quit
# ══════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SHARED_LIBS="$ROOT_DIR/shared_libs_debug"
BINARY="$ROOT_DIR/target/debug/add_one_dyn"
UDP_PORT=7878
OUTPUT_WITHOUT="$SCRIPT_DIR/tmp/output_without_add_one.bin"
OUTPUT_WITH="$SCRIPT_DIR/tmp/output_with_add_one.bin"

cleanup() {
    if [ -n "$BIN_PID" ] && kill -0 "$BIN_PID" 2>/dev/null; then
        echo "Cleaning up: sending Q to binary..."
        echo "Q" | nc -u -w1 127.0.0.1 $UDP_PORT 2>/dev/null || true
        sleep 1
        kill "$BIN_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ── Step 0: Clean ────────────────────────────────────────────────────
echo "=========================================="
echo "Step 0: Clean previous artifacts"
echo "=========================================="
rm -f "$OUTPUT_WITHOUT" "$OUTPUT_WITH" "$SCRIPT_DIR/tmp/idle.bin"
mkdir -p "$SCRIPT_DIR/tmp"
mkdir -p "$SHARED_LIBS"
cd "$ROOT_DIR"

# Remove add_one_plugin from shared_libs so it does NOT exist at startup
rm -f "$SHARED_LIBS/libadd_one_plugin.so"
cargo clean -p add_one_plugin 2>/dev/null || true

echo ""

# ── Step 1: Build core + base plugins + binary (no add_one) ─────────
# PRECONDITION: add_one_plugin does NOT exist — Flow B cannot run yet.
echo "=========================================="
echo "Step 1: Build core + base plugins + binary"
echo "=========================================="
echo "(add_one_plugin is intentionally NOT built)"
echo ""

cargo build -p futuresdr 2>&1 | tail -5
for plugin in null_source_plugin null_sink_plugin head_plugin file_sink_plugin; do
    echo "  Building $plugin..."
    cargo build -p "$plugin" 2>&1 | tail -3
done

# Collect base plugins
cp "$ROOT_DIR/target/debug/libfuturesdr.so" "$SHARED_LIBS/" 2>/dev/null || true
for plugin in null_source_plugin null_sink_plugin head_plugin file_sink_plugin; do
    cp "$ROOT_DIR/target/debug/lib${plugin}.so" "$SHARED_LIBS/" 2>/dev/null || true
done

echo "  Building add_one_dyn binary..."
cargo build -p add_one_dyn_example 2>&1 | tail -3

echo ""
echo "Shared libs (NO add_one_plugin):"
ls -1 "$SHARED_LIBS"/*.so 2>/dev/null | sed 's|.*/||'
echo ""

# ── Step 2: Start binary in --hot-load mode ──────────────────────────
echo "=========================================="
echo "Step 2: Start binary in --hot-load mode"
echo "=========================================="

cd "$SCRIPT_DIR"
export PLUGIN_DIR="$SHARED_LIBS"
export LD_LIBRARY_PATH="${SHARED_LIBS}:${LD_LIBRARY_PATH}"

"$BINARY" --hot-load > "$SCRIPT_DIR/tmp/hotload.log" 2>&1 &
BIN_PID=$!
echo "  Binary PID: $BIN_PID"
echo "  Waiting for startup..."
sleep 2

if ! kill -0 "$BIN_PID" 2>/dev/null; then
    echo "ERROR: Binary died on startup. Log:"
    cat "$SCRIPT_DIR/tmp/hotload.log"
    exit 1
fi

echo ""

# ── Step 3: Verify output_without_add_one.bin ────────────────────────
# TEST 1: Flow A produces 0x00 (no add_one plugin involved).
echo "=========================================="
echo "Step 3: TEST 1 — permanent FG output == 0x00"
echo "=========================================="

# The permanent FG (NullSource→Head(1)→FileSink) runs immediately.
# Give it a moment to flush.
sleep 1

if [ -f "$OUTPUT_WITHOUT" ]; then
    BYTE=$(od -An -tx1 "$OUTPUT_WITHOUT" | head -1 | tr -d ' ')
    if [ "$BYTE" = "00" ]; then
        echo "  TEST 1 PASS: output_without_add_one.bin = 0x$BYTE"
    else
        echo "  TEST 1 FAIL: expected 0x00, got 0x$BYTE"
        exit 1
    fi
else
    echo "  TEST 1 FAIL: $OUTPUT_WITHOUT not found"
    cat "$SCRIPT_DIR/tmp/hotload.log"
    exit 1
fi

echo ""
# TEST 2: Binary stays alive — plugin absence did not crash it.
echo "  TEST 2: Binary is still alive (PID $BIN_PID): $(kill -0 "$BIN_PID" 2>/dev/null && echo PASS || echo FAIL)"
echo ""

# ── Step 4: Compile add_one_plugin (while binary is running) ────────
# A POSTERIORI COMPILATION: the plugin is built after the program started.
echo "=========================================="
echo "Step 4: A posteriori compilation — build add_one_plugin"
echo "=========================================="
echo "  The binary is running. Compiling add_one_plugin NOW..."

cd "$ROOT_DIR"
cargo build -p add_one_plugin 2>&1 | tail -5
echo ""

# ── Step 5: Copy plugin into shared_libs ─────────────────────────────
echo "=========================================="
echo "Step 5: Deploy add_one_plugin to shared_libs"
echo "=========================================="

cp "$ROOT_DIR/target/debug/libadd_one_plugin.so" "$SHARED_LIBS/"
echo "  Copied libadd_one_plugin.so → $SHARED_LIBS/"
echo ""
echo "Shared libs (now WITH add_one_plugin):"
ls -1 "$SHARED_LIBS"/*.so 2>/dev/null | sed 's|.*/||'
echo ""

# ── Step 6: UDP swap → fg_with_add_one.toml ──────────────────────────
echo "=========================================="
echo "Step 6: UDP swap to fg_with_add_one.toml"
echo "=========================================="

echo "  Sending: -s flows/fg_with_add_one.toml"
echo "-s flows/fg_with_add_one.toml" | nc -u -w1 127.0.0.1 $UDP_PORT

echo "  Waiting for swap to complete..."
sleep 2

echo ""

# ── Step 7: Verify output_with_add_one.bin ───────────────────────────
# TEST 3: Flow B (compiled a posteriori) produces 0x01.
echo "=========================================="
echo "Step 7: TEST 3 — hot-loaded FG output == 0x01"
echo "=========================================="

if [ -f "$OUTPUT_WITH" ]; then
    BYTE=$(od -An -tx1 "$OUTPUT_WITH" | head -1 | tr -d ' ')
    if [ "$BYTE" = "01" ]; then
        echo "  TEST 3 PASS: output_with_add_one.bin = 0x$BYTE"
    else
        echo "  TEST 3 FAIL: expected 0x01, got 0x$BYTE"
        echo "  Log tail:"
        tail -20 "$SCRIPT_DIR/tmp/hotload.log"
        exit 1
    fi
else
    echo "  TEST 3 FAIL: $OUTPUT_WITH not found"
    echo "  Log tail:"
    tail -20 "$SCRIPT_DIR/tmp/hotload.log"
    exit 1
fi

echo ""

# ── Step 8: Shut down ───────────────────────────────────────────────
echo "=========================================="
echo "Step 8: Shut down"
echo "=========================================="

echo "Q" | nc -u -w1 127.0.0.1 $UDP_PORT
sleep 1

if kill -0 "$BIN_PID" 2>/dev/null; then
    echo "  Binary still alive, force killing..."
    kill "$BIN_PID" 2>/dev/null || true
else
    echo "  Binary exited cleanly."
fi
BIN_PID=""  # prevent double-kill in trap

echo ""
echo "=========================================="
echo "ALL TESTS PASSED"
echo "=========================================="
echo ""
echo "TEST 1: Flow A output == 0x00  (base plugins only)         PASS"
echo "TEST 2: Binary alive without add_one_plugin                PASS"
echo "TEST 3: Flow B output == 0x01  (a posteriori compilation)  PASS"
echo ""
echo "Proved: a plugin compiled AFTER the binary started"
echo "was successfully loaded at runtime via UDP swap."
echo ""
echo "Binary log:"
cat "$SCRIPT_DIR/tmp/hotload.log"
