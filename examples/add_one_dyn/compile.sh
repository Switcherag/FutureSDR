#!/bin/bash
set -e

# Compilation script for add_one_dyn example
# This script compiles everything from the root to ensure same ABI
# The key is: compile core first, then plugin, ensuring same toolchain

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXAMPLE_DIR="$SCRIPT_DIR"
SHARED_LIBS="$ROOT_DIR/shared_libs_debug"

cd "$ROOT_DIR"

echo "=========================================="
echo "Step 1: Clean previous builds"
echo "=========================================="
# Clean workspace artifacts for plugin and example (shared target/ at root)
cargo clean -p add_one_plugin 2>/dev/null || true
cargo clean -p add_one_dyn_example 2>/dev/null || true
echo ""
echo "=========================================="
echo "Step 2: Build add_one plugin (ABI-compatible with core)"
echo "=========================================="
# Both plugin and example are workspace members → shared target/ at root
cargo build -p add_one_plugin

echo ""
echo "=========================================="
echo "Step 3: Build the add_one_dyn example binary"
echo "=========================================="
cargo build -p add_one_dyn_example

echo ""
echo "=========================================="
echo "Step 4: Collect plugins and dependencies"
echo "=========================================="
mkdir -p "$SHARED_LIBS"

# Copy libfuturesdr.so
cp "$ROOT_DIR/target/debug/libfuturesdr.so" "$SHARED_LIBS/" 2>/dev/null || \
    echo "  Note: libfuturesdr.so not found (may be integrated)"

# Copy add_one plugin — workspace builds output to the root target/
cp "$ROOT_DIR/target/debug/libadd_one_plugin.so" "$SHARED_LIBS/" || \
    (echo "Error: Failed to copy add_one plugin"; exit 1)

# Copy other required plugins
for plugin_dir in null_source_plugin null_sink_plugin head_plugin file_sink_plugin; do
    if [ -f "$ROOT_DIR/target/debug/lib${plugin_dir}.so" ]; then
        cp "$ROOT_DIR/target/debug/lib${plugin_dir}.so" "$SHARED_LIBS/" 2>/dev/null || true
    fi
done

echo ""
echo "=========================================="
echo "Compilation Summary"
echo "=========================================="
echo "✓ add_one plugin compiled"
echo "✓ add_one_dyn example compiled"
echo ""
echo "Binaries and plugins collected:"
ls -lh "$SHARED_LIBS"/*.so 2>/dev/null || echo "  (no .so files found)"
echo ""
echo "Example binary: $ROOT_DIR/target/debug/add_one_dyn"
echo ""
echo "To run tests, execute: bash $SCRIPT_DIR/run_tests.sh"
