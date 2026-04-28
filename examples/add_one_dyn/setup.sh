#!/bin/bash
set -e

# Setup script - Run this ONCE before running compile.sh
# This builds the FutureSDR core and all required plugins
# Must be run from the repository root

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Extract the ABI detail string embedded in a .so file.
extract_abi_signature() {
    local so_path="$1"
    if [ ! -f "$so_path" ]; then
        echo "signature not found"
        return
    fi

    strings "$so_path" | awk '
        index($0,"rustc=") && index($0,"futuresdr=") && index($0,"target=") && index($0,"seify=") {
            sub(/unsafe precondition.*/, "");
            print;
            exit;
        }
    '
}

cd "$ROOT_DIR"

echo "=========================================="
echo "FutureSDR Setup (Core Build)"
echo "=========================================="
echo ""
echo "This builds the core and all required plugins."
echo "Run this ONCE, then you can run compile.sh multiple times."
echo ""

echo "=========================================="
echo "Building FutureSDR core..."
echo "=========================================="
cargo build -p futuresdr

echo ""
echo "=========================================="
echo "Building required plugins..."
echo "=========================================="
for plugin in null_source_plugin null_sink_plugin head_plugin file_sink_plugin; do
    echo "Building $plugin..."
    cargo build -p "$plugin" 2>/dev/null || echo "  (plugin may not exist or already built)"
done

echo ""
echo "=========================================="
echo "ABI Signatures"
echo "=========================================="

FUTURESDR_SO="$ROOT_DIR/target/debug/libfuturesdr.so"
echo "futuresdr: $(extract_abi_signature "$FUTURESDR_SO")"

for plugin in null_source_plugin null_sink_plugin head_plugin file_sink_plugin add_one_plugin; do
    so="$ROOT_DIR/target/debug/lib${plugin}.so"
    echo "${plugin}: $(extract_abi_signature "$so")"
done

echo ""
echo "=========================================="
echo "Setup Complete!"
echo "=========================================="
echo ""
echo "Now you can run:"
echo "  bash $SCRIPT_DIR/compile.sh"
