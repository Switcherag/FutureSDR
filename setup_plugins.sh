#!/bin/bash
# ──────────────────────────────────────────────────────────────────
# FutureSDR plugin build system
# ──────────────────────────────────────────────────────────────────
#
# Flags are handled automatically:
#   • -C prefer-dynamic  → set in .cargo/config.toml
#   • RPATH ($ORIGIN)    → set in plugin_build.rs (binary examples)
#   • No LD_LIBRARY_PATH needed
#
# Usage:
#   ./setup_plugins.sh                    Build all plugins (debug)
#   ./setup_plugins.sh --release          Build all plugins (release)
#   ./setup_plugins.sh --lib              Rebuild libfuturesdr.so only
#   ./setup_plugins.sh --examples         Build dyn example binaries only
#   ./setup_plugins.sh --all              Build lib + all plugins + examples
#   ./setup_plugins.sh --sdk              Package plugin SDK
#   ./setup_plugins.sh throttle_plugin    Build one plugin
#   ./setup_plugins.sh cross-flowgraph-example   Build one example
#   ./setup_plugins.sh p1 p2 p3          Build specific packages
#
# Combine flags:
#   ./setup_plugins.sh --release --all    Full release build
#   ./setup_plugins.sh --release --lib    Release libfuturesdr.so only
#   ./setup_plugins.sh --release throttle_plugin   One plugin, release
#
# A-posteriori plugin builds:
#   After an initial --all (or --lib) build, new plugins can be built
#   individually.  They link against the existing libfuturesdr.so and
#   are ABI-checked at load time.
#
set -e

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
cd "$ROOT_DIR"

SHARED_DIR="$ROOT_DIR/shared_libs"

# ── Parse arguments ──────────────────────────────────────────────
RELEASE=false
BUILD_LIB=false
BUILD_PLUGINS=false
BUILD_EXAMPLES=false
BUILD_SDK=false
BUILD_ALL=false
PACKAGES=()

for arg in "$@"; do
    case "$arg" in
        --release)   RELEASE=true ;;
        --lib)       BUILD_LIB=true ;;
        --examples)  BUILD_EXAMPLES=true ;;
        --all)       BUILD_ALL=true ;;
        --sdk)       BUILD_SDK=true ;;
        --help|-h)
            sed -n '/^#/!q;s/^# \?//;p' "$0" | tail -n +2
            exit 0
            ;;
        -*)
            echo "Unknown flag: $arg (try --help)" >&2
            exit 1
            ;;
        *)
            PACKAGES+=("$arg")
            ;;
    esac
done

# Default: if no flags and no packages → build all plugins
if ! $BUILD_LIB && ! $BUILD_EXAMPLES && ! $BUILD_ALL && ! $BUILD_SDK && [ ${#PACKAGES[@]} -eq 0 ]; then
    BUILD_PLUGINS=true
fi

if $BUILD_ALL; then
    BUILD_LIB=true
    BUILD_PLUGINS=true
    BUILD_EXAMPLES=true
fi

# ── Cargo profile ───────────────────────────────────────────────
PROFILE_FLAG=""
PROFILE_NAME="debug"
if $RELEASE; then
    PROFILE_FLAG="--release"
    PROFILE_NAME="release"
fi

TARGET_DIR="target/$PROFILE_NAME"

# ── Auto-discover plugins from plugins/ directory ────────────────
discover_plugins() {
    local plugins=()
    for d in "$ROOT_DIR"/plugins/*/Cargo.toml; do
        local name
        name=$(basename "$(dirname "$d")")
        plugins+=("$name")
    done
    echo "${plugins[@]}"
}

# ── Dyn example binaries ────────────────────────────────────────
DYN_EXAMPLES=(
    dylib-example
    dyn_fm-receiver
    dyn_phy-swap
    cross-flowgraph-example
    sdr_tuning
)

# ── Build helpers ────────────────────────────────────────────────
build_package() {
    local pkg="$1"
    echo "  Building $pkg ..."
    if ! cargo build $PROFILE_FLAG -p "$pkg" 2>&1; then
        echo "  SKIP $pkg (build failed)"
        return 1
    fi
}

# ── Build libfuturesdr.so ────────────────────────────────────────
if $BUILD_LIB; then
    echo "=== Building futuresdr ($PROFILE_NAME) ==="
    cargo build $PROFILE_FLAG -p futuresdr 2>&1
    echo ""
fi

# ── Build all plugins ───────────────────────────────────────────
if $BUILD_PLUGINS; then
    echo "=== Building all plugins ($PROFILE_NAME) ==="
    DISCOVERED=($(discover_plugins))
    echo "  Found ${#DISCOVERED[@]} plugins"
    FAILED=()
    for p in "${DISCOVERED[@]}"; do
        build_package "$p" || FAILED+=("$p")
    done
    if [ ${#FAILED[@]} -gt 0 ]; then
        echo ""
        echo "  Skipped (${#FAILED[@]}): ${FAILED[*]}"
    fi
    echo ""
fi

# ── Build example binaries ──────────────────────────────────────
if $BUILD_EXAMPLES; then
    echo "=== Building dyn examples ($PROFILE_NAME) ==="
    for ex in "${DYN_EXAMPLES[@]}"; do
        build_package "$ex" || true
    done
    echo ""
fi

# ── Build specific packages ─────────────────────────────────────
if [ ${#PACKAGES[@]} -gt 0 ]; then
    echo "=== Building specified packages ($PROFILE_NAME) ==="
    for pkg in "${PACKAGES[@]}"; do
        build_package "$pkg"
    done
    echo ""
fi

# ── Collect shared libs ─────────────────────────────────────────
mkdir -p "$SHARED_DIR/$PROFILE_NAME"
cp "$TARGET_DIR"/libfuturesdr.so "$SHARED_DIR/$PROFILE_NAME/" 2>/dev/null || true
cp "$TARGET_DIR"/lib*_plugin.so  "$SHARED_DIR/$PROFILE_NAME/" 2>/dev/null || true

# Copy Rust stdlib
SYSROOT=$(rustc --print sysroot 2>/dev/null)
TARGET_TRIPLE=$(rustc -vV 2>/dev/null | grep host | cut -d' ' -f2)
RUST_STDLIB="$SYSROOT/lib/rustlib/$TARGET_TRIPLE/lib/$(ls "$SYSROOT/lib/rustlib/$TARGET_TRIPLE/lib/" 2>/dev/null | grep '^libstd-.*\.so$' | head -1)"
if [ -f "$RUST_STDLIB" ]; then
    cp "$RUST_STDLIB" "$SHARED_DIR/$PROFILE_NAME/"
fi

LIB_COUNT=$(ls "$SHARED_DIR/$PROFILE_NAME/"*.so 2>/dev/null | wc -l)
echo "=== Collected $LIB_COUNT .so files in $SHARED_DIR/$PROFILE_NAME/ ==="

# ── Package Plugin SDK ──────────────────────────────────────────
if $BUILD_SDK; then
    SDK_DIR="$ROOT_DIR/plugin_sdk"
    echo ""
    echo "=== Packaging Plugin SDK ==="
    mkdir -p "$SDK_DIR"

    # libfuturesdr.so (from the profile just built)
    cp "$TARGET_DIR/libfuturesdr.so" "$SDK_DIR/" 2>/dev/null || true

    # Rust stdlib
    [ -f "$RUST_STDLIB" ] && cp "$RUST_STDLIB" "$SDK_DIR/"

    # Toolchain file
    cp "$ROOT_DIR/rust-toolchain.toml" "$SDK_DIR/" 2>/dev/null || true

    # ABI manifest
    RUSTC_VERSION=$(rustc --version 2>/dev/null || echo "unknown")
    FUTURESDR_VERSION=$(grep '^version' "$ROOT_DIR/Cargo.toml" | head -1 | cut -d'"' -f2)
    cat > "$SDK_DIR/manifest.json" <<MANEOF
{
  "rustc_version": "$RUSTC_VERSION",
  "futuresdr_version": "$FUTURESDR_VERSION",
  "target": "$TARGET_TRIPLE",
  "profile": "$PROFILE_NAME",
  "created": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
MANEOF

    # Plugin template Cargo.toml
    cat > "$SDK_DIR/plugin_template_Cargo.toml" <<'TPLEOF'
[package]
name = "my_plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["dylib"]

[dependencies]
plugin_api = { path = "<path-to-futuresdr>/crates/plugin_api" }
futuresdr = { path = "<path-to-futuresdr>", features = ["plugin"] }
TPLEOF

    echo "  SDK at: $SDK_DIR/"
    echo "  Contents: $(ls "$SDK_DIR/" | tr '\n' ' ')"
fi

# ── Summary ─────────────────────────────────────────────────────
echo ""
echo "Done.  Binaries are in $TARGET_DIR/"
echo "Run directly — no LD_LIBRARY_PATH or PLUGIN_DIR needed:"
echo "  ./$TARGET_DIR/cross_fg"
