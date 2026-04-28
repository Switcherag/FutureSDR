#!/bin/bash
set -e

# Test script for add_one_dyn example
# Tests the flowgraph with and without add_one plugin

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Find the compiled binary
if [ -f "$ROOT_DIR/target/debug/add_one_dyn" ]; then
    BINARY="$ROOT_DIR/target/debug/add_one_dyn"
    PROFILE="debug"
elif [ -f "$ROOT_DIR/target/release/add_one_dyn" ]; then
    BINARY="$ROOT_DIR/target/release/add_one_dyn"
    PROFILE="release"
else
    echo "Error: Binary not found"
    echo "  Checked: $ROOT_DIR/target/debug/add_one_dyn"
    echo "  Checked: $ROOT_DIR/target/release/add_one_dyn"
    exit 1
fi

PLUGIN_DIR="$ROOT_DIR/shared_libs_debug"
OUTPUT_WITHOUT_ADD_ONE="$SCRIPT_DIR/tmp/output_without_add_one.bin"
OUTPUT_WITH_ADD_ONE="$SCRIPT_DIR/tmp/output_with_add_one.bin"

echo "=========================================="
echo "Test Setup"
echo "=========================================="
echo "Binary: $BINARY"
echo "Plugin dir: $PLUGIN_DIR"
echo "Output (without): $OUTPUT_WITHOUT_ADD_ONE"
echo "Output (with): $OUTPUT_WITH_ADD_ONE"
echo ""

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    echo "Error: Binary not found at $BINARY"
    echo "Please run: bash $SCRIPT_DIR/compile.sh"
    exit 1
fi

# Clean previous outputs
rm -f "$OUTPUT_WITHOUT_ADD_ONE" "$OUTPUT_WITH_ADD_ONE"
mkdir -p "$SCRIPT_DIR/tmp"

echo "=========================================="
echo "Test 1: Run WITHOUT add_one plugin"
echo "=========================================="

cd "$SCRIPT_DIR"  # Run from example directory to find flows/

# Set environment for plugin discovery — single directory, not a colon-separated path
export PLUGIN_DIR
export LD_LIBRARY_PATH="${PLUGIN_DIR}:${LD_LIBRARY_PATH}"

echo "Running: $BINARY"
"$BINARY" > /tmp/test1.log 2>&1 || true
cat /tmp/test1.log | head -30

if [ -f "$OUTPUT_WITHOUT_ADD_ONE" ]; then
    SIZE=$(stat -c%s "$OUTPUT_WITHOUT_ADD_ONE" 2>/dev/null)
    FIRST_BYTE=$(od -An -tx1 "$OUTPUT_WITHOUT_ADD_ONE" 2>/dev/null | head -1 | tr -d ' ')
    
    echo "✓ Output file created"
    echo "  File size: $SIZE bytes"
    echo "  First byte (hex): 0x$FIRST_BYTE"
    
    if [ "$FIRST_BYTE" = "00" ]; then
        echo "✓ Test 1 PASSED: Without add_one, value is 0 (0x$FIRST_BYTE)"
        TEST1_RESULT=0
    else
        echo "✗ Test 1 FAILED: Expected first byte 0x00, got 0x$FIRST_BYTE"
        TEST1_RESULT=1
    fi
else
    echo "✗ Test 1 FAILED: Output file not created at $OUTPUT_WITHOUT_ADD_ONE"
    ls /tmp/output* 2>/dev/null || echo "No output files in /tmp"
    TEST1_RESULT=1
fi

echo ""
echo "=========================================="
echo "Test 2: Run WITH add_one plugin"
echo "=========================================="

echo "Running: $BINARY --add-one"
"$BINARY" --add-one > /tmp/test2.log 2>&1 || true
cat /tmp/test2.log | head -30

if [ -f "$OUTPUT_WITH_ADD_ONE" ]; then
    SIZE=$(stat -c%s "$OUTPUT_WITH_ADD_ONE" 2>/dev/null)
    FIRST_BYTE=$(od -An -tx1 "$OUTPUT_WITH_ADD_ONE" 2>/dev/null | head -1 | tr -d ' ')
    
    echo "✓ Output file created"
    echo "  File size: $SIZE bytes"
    echo "  First byte (hex): 0x$FIRST_BYTE"
    
    if [ "$FIRST_BYTE" = "01" ]; then
        echo "✓ Test 2 PASSED: With add_one, value is 1 (0x$FIRST_BYTE)"
        TEST2_RESULT=0
    else
        echo "✗ Test 2 FAILED: Expected first byte 0x01, got 0x$FIRST_BYTE"
        TEST2_RESULT=1
    fi
else
    echo "✗ Test 2 FAILED: Output file not created at $OUTPUT_WITH_ADD_ONE"
    ls /tmp/output* 2>/dev/null || echo "No output files in /tmp"
    TEST2_RESULT=1
fi

echo ""
echo "=========================================="
echo "Test Summary"
echo "=========================================="
if [ $TEST1_RESULT -eq 0 ] && [ $TEST2_RESULT -eq 0 ]; then
    echo "✓ All tests PASSED!"
    exit 0
else
    echo "✗ Some tests FAILED"
    echo ""
    echo "Debugging info:"
    echo "  Plugin directory: $PLUGIN_DIR"
    ls -la "$PLUGIN_DIR"/*.so 2>/dev/null || echo "    (no .so files)"
    exit 1
fi
