#!/usr/bin/env bash
# Test pipeline of the plugin add-on. CI runs the same stages.
#
#   ./ci.sh                  fmt lint test plugins sdk size
#   ./ci.sh all              every stage
#   ./ci.sh <stage>...       the given stages, in that order
#
# Stages
#   fmt       rustfmt: this workspace, the plugin crates in blocks/, the
#             vendored crate and the core files the add-on changed
#   lint      clippy -D warnings; the plugin crates through the SDK with
#             --clippy --deny-warnings; rustdoc warnings; unused dependencies
#             (if cargo-machete is installed)
#   test      unit and integration tests, debug profile
#   plugins   the tests of the plugin crates in blocks/ (receivers against
#             recordings), through a debug SDK; with WLAN_HALOW_RECORDING,
#             also the one-second HaLow recording, through a release SDK
#   release   the same with the release profile (one codegen unit, stripped),
#             and the bridge throughput measurement
#   sdk       end to end: pack an SDK from a release build, move it, build
#             plugins from it, run the examples against them
#   size      size budgets, and no local-domain code in plugins of
#             non-blocking blocks
#   core      FutureSDR tests of what the add-on changed in the core
#   vendor    tests of vendor/vmcircbuffer, plain and with AddressSanitizer
#   miri      the plugin API's unit tests under Miri
#   stress    randomized tests with CI_SCALE (default 20) times the cases,
#             then the controller and link tests CI_REPEAT (default 5) times
#             with every core busy
#   coverage  line coverage of api, host and sdk, at least CI_MIN_COVERAGE
#             percent (default 90); HTML report in target/ci/coverage/html
#             (needs llvm-tools)
#
# Randomized tests print PLUGIN_TEST_SEED=... when they fail; set it to
# replay the case. PLUGIN_TEST_SEED_BASE draws other cases.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE="$(cd "$ROOT/../.." && pwd)"
CI="$ROOT/target/ci"
cd "$ROOT"

export FUTURESDR_log_level=warn
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}"
HOST="$(rustc -vV | sed -n 's/^host: //p')"

BUSY=()
trap '[ ${#BUSY[@]} -eq 0 ] || kill "${BUSY[@]}" 2>/dev/null' EXIT

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() { printf '\033[31mFAILED: %s\033[0m\n' "$*" >&2; exit 1; }

# Run a release example; cargo sets the library path for the shared library.
example() {
    local name=$1
    shift
    cargo run -q --release -p futuresdr-plugin-host --example "$name" -- "$@"
}

fsdr_plugin() { "$ROOT/target/release/fsdr-plugin" "$@"; }

# Build the release examples and fsdr-plugin, and pack an SDK from the
# shared library the examples use, into $CI/e2e/sdk. Once per run.
RT=""
release_sdk() {
    [ -n "$RT" ] && return
    # Other builds (e.g. the release tests) may leave another copy of the
    # library in target/release: take the path from this build.
    RT=$(cargo build --release -p futuresdr-plugin-host --examples --message-format=json |
        grep '"name":"futuresdr_plugin_rt"' | grep -o '"[^"]*libfuturesdr_plugin_rt\.so"' |
        tr -d '"' | head -n 1)
    [ -f "$RT" ] || fail "no shared library in the example build"
    cargo build -q --release -p futuresdr-plugin-sdk --bin fsdr-plugin
    rm -rf "$CI/e2e"
    mkdir -p "$CI/e2e"
    fsdr_plugin pack --from "$RT" --out "$CI/e2e/packed"
    # An SDK is a directory that can be moved.
    mv "$CI/e2e/packed" "$CI/e2e/sdk"
}

# The plugin crates.
PLUGINS=()
for manifest in "$ROOT"/blocks/*/Cargo.toml; do
    PLUGINS+=("$(dirname "$manifest")")
done

# Pack an SDK from the debug build into $CI/dev-sdk. Once per run.
DEV_SDK=""
dev_sdk() {
    [ -n "$DEV_SDK" ] && return
    cargo build -q -p futuresdr-plugin-rt -p futuresdr-plugin-sdk
    rm -rf "$CI/dev-sdk"
    "$ROOT/target/debug/fsdr-plugin" pack \
        --from "$ROOT/target/debug/libfuturesdr_plugin_rt.so" --out "$CI/dev-sdk" >/dev/null
    DEV_SDK="$CI/dev-sdk"
}

# A plugin crate with one block type, in directory $1.
tiny_plugin() {
    mkdir -p "$1/src"
    cat >"$1/Cargo.toml" <<'EOF'
[package]
name = "tiny"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["dylib"]

[workspace]
EOF
    cat >"$1/src/lib.rs" <<'EOF'
extern crate futuresdr_plugin_rt as futuresdr;

use futuresdr::prelude::*;

export_plugin! {
    name: "tiny",
    blocks: [
        {
            name: "Tiny",
            types: [u8],
            description: "Copy.",
            add: |_s| blocks::Copy::<T>::new(),
        },
    ]
}
EOF
}

stage_fmt() {
    step "fmt"
    cargo fmt --all --check
    for plugin in "${PLUGINS[@]}"; do
        cargo fmt --check --manifest-path "$plugin/Cargo.toml"
    done
    cargo fmt --check --manifest-path vendor/vmcircbuffer/Cargo.toml
    (cd "$CORE" && rustfmt --edition 2024 --check \
        src/runtime/flowgraph.rs src/runtime/kernel_interface.rs \
        crates/macros/src/lib.rs tests/local_domain.rs)
}

stage_lint() {
    step "clippy"
    cargo clippy -q --workspace --all-targets -- -D warnings

    step "clippy: plugin crates, through a debug SDK"
    dev_sdk
    for plugin in "${PLUGINS[@]}"; do
        echo "  ${plugin#"$ROOT"/}"
        "$ROOT/target/debug/fsdr-plugin" build --sdk "$DEV_SDK" "$plugin" \
            --target-dir "$CI/lint" --clippy --deny-warnings >/dev/null
    done

    step "rustdoc"
    RUSTDOCFLAGS="-D warnings" cargo doc -q --workspace --no-deps

    step "unused dependencies"
    if command -v cargo-machete >/dev/null; then
        cargo machete
    else
        echo "cargo-machete is not installed: skipped"
    fi
}

stage_test() {
    step "tests (debug)"
    cargo test -q --workspace
}

stage_plugins() {
    dev_sdk
    for plugin in "${PLUGINS[@]}"; do
        step "plugin tests: ${plugin#"$ROOT"/}"
        "$ROOT/target/debug/fsdr-plugin" test --sdk "$DEV_SDK" "$plugin" \
            --target-dir "$CI/plugins" -- --quiet
    done
    if [ -n "${WLAN_HALOW_RECORDING:-}" ]; then
        step "plugin tests: HaLow recording, release"
        release_sdk
        fsdr_plugin test --sdk "$CI/e2e/sdk" blocks/wlan --target-dir "$CI/plugins" \
            -- --ignored --nocapture ah_long_recording
    fi
}

stage_release() {
    step "tests (release)"
    cargo test -q --workspace --release
    step "bridge throughput"
    cargo test -q --release -p futuresdr-plugin-host --lib -- --ignored --nocapture throughput
}

stage_sdk() {
    step "sdk: pack, move, build, load"
    release_sdk
    mkdir -p "$CI/e2e/plugins"
    fsdr_plugin info --sdk "$CI/e2e/sdk"
    fsdr_plugin build --sdk "$CI/e2e/sdk" blocks/basic --target-dir "$CI/e2e/target" \
        --deny-warnings >/dev/null
    cp "$CI/e2e/target/release/libfsdr_blocks_basic.so" "$CI/e2e/plugins/"

    step "sdk: examples against the plugins"
    example swap_receivers --plugins "$CI/e2e/plugins" --hold keep
    example swap_receivers --plugins "$CI/e2e/plugins" --hold discard
    for mode in on-demand standby; do
        for driver in task thread; do
            example swap_bench --plugins "$CI/e2e/plugins" --iterations 10 --settle-ms 10 \
                --mode "$mode" --driver "$driver" --rate 100000
        done
    done
}

# check <what> <file> <max bytes>
check_size() {
    local size
    size=$(stat -c %s "$2")
    printf '  %-34s %9d bytes (budget %d)\n' "$1" "$size" "$3"
    [ "$size" -le "$3" ] || fail "$1 is over its size budget"
}

stage_size() {
    step "size budgets"
    release_sdk
    rm -rf "$CI/size"
    tiny_plugin "$CI/size/tiny"
    local tiny basic wlan zigbee
    tiny=$(fsdr_plugin build --sdk "$CI/e2e/sdk" "$CI/size/tiny" --target-dir "$CI/size/target")
    basic=$(fsdr_plugin build --sdk "$CI/e2e/sdk" blocks/basic --target-dir "$CI/size/target")
    wlan=$(fsdr_plugin build --sdk "$CI/e2e/sdk" blocks/wlan --target-dir "$CI/size/target")
    zigbee=$(fsdr_plugin build --sdk "$CI/e2e/sdk" blocks/zigbee --target-dir "$CI/size/target")
    check_size "shared library" "$RT" 6000000
    check_size "one-block plugin" "$tiny" 150000
    check_size "basic plugin (74 block types)" "$basic" 2000000
    check_size "wlan plugin (802.11a and ah)" "$wlan" 500000
    check_size "zigbee plugin" "$zigbee" 500000
    check_size "swap_bench" "$ROOT/target/release/examples/swap_bench" 2000000

    step "no local-domain code in plugins of non-blocking blocks"
    CARGO_PROFILE_RELEASE_STRIP=none fsdr_plugin build --sdk "$CI/e2e/sdk" blocks/basic \
        --target-dir "$CI/size/unstripped" >/dev/null
    local local_runs
    local_runs=$(nm -C "$CI/size/unstripped/release/libfsdr_blocks_basic.so" |
        grep -c 'as futuresdr::runtime::block::LocalBlock>::run' || true)
    echo "  LocalBlock::run instances: $local_runs"
    [ "$local_runs" -eq 0 ] || fail "plugins instantiate the local-domain block path"
}

stage_core() {
    step "FutureSDR: tests of the core changes"
    (cd "$CORE" && cargo test -q -p futuresdr --lib --test local_domain &&
        cargo test -q --manifest-path crates/macros/Cargo.toml)
}

stage_vendor() {
    local manifest="$ROOT/vendor/vmcircbuffer/Cargo.toml"
    step "vendored vmcircbuffer"
    cargo test -q --manifest-path "$manifest" --features sync,async,nonblocking,lockfree \
        --target-dir "$CI/vendor"
    step "vendored vmcircbuffer, AddressSanitizer"
    RUSTFLAGS="-Zsanitizer=address" RUSTDOCFLAGS="-Zsanitizer=address" \
        cargo test -q --manifest-path "$manifest" --features sync,async,nonblocking,lockfree \
        --target "$HOST" --target-dir "$CI/vendor-asan" --lib --tests
    rm -f "$ROOT/vendor/vmcircbuffer/Cargo.lock"
}

stage_miri() {
    step "Miri: plugin API"
    # Miri cannot build dylibs, so crates that link the shared library (the
    # host) are out of its reach; they have no unsafe code. The vendored
    # crate's unsafe mapping code, which Miri cannot emulate either, runs
    # under AddressSanitizer (stage vendor).
    cargo miri test -q -p futuresdr-plugin-api --lib
}

stage_stress() {
    local scale="${CI_SCALE:-20}" repeat="${CI_REPEAT:-5}"
    step "randomized tests, $scale times the cases"
    PLUGIN_TEST_CASES="$scale" cargo test -q --workspace --release

    step "controller and link tests, $repeat times, every core busy"
    cargo test -q --release -p futuresdr-plugin-host --test controller --test links --no-run
    local i status=0
    BUSY=()
    for ((i = 0; i < $(nproc); i++)); do
        (while :; do :; done) &
        BUSY+=($!)
    done
    for ((i = 1; i <= repeat; i++)); do
        echo "  run $i"
        timeout 600 cargo test -q --release -p futuresdr-plugin-host --test controller \
            --test links ||
            { status=$?; break; }
    done
    kill "${BUSY[@]}" 2>/dev/null || true
    BUSY=()
    [ "$status" -eq 0 ] || fail "controller tests under load (exit $status)"
}

stage_coverage() {
    local tools profdata cov
    tools="$(rustc --print sysroot)/lib/rustlib/$HOST/bin"
    profdata="$tools/llvm-profdata"
    cov="$tools/llvm-cov"
    [ -x "$cov" ] || fail "llvm-tools are not installed (rustup component add llvm-tools)"
    step "coverage"
    rm -rf "$CI/coverage"
    mkdir -p "$CI/coverage/profiles"
    export RUSTFLAGS="-C instrument-coverage"
    export LLVM_PROFILE_FILE="$CI/coverage/profiles/%p-%m.profraw"
    local target="$CI/coverage/target"
    cargo test -q --workspace --target-dir "$target"
    local objects=()
    while IFS= read -r exe; do
        objects+=(--object "$exe")
    done < <(cargo test -q --workspace --target-dir "$target" --no-run \
        --message-format=json 2>/dev/null |
        sed -n 's/.*"executable":"\([^"]*\)".*/\1/p')
    # The command line runs as a process of its own.
    objects+=(--object "$target/debug/fsdr-plugin")
    unset RUSTFLAGS LLVM_PROFILE_FILE
    "$profdata" merge -sparse "$CI"/coverage/profiles/*.profraw -o "$CI/coverage/merged.profdata"
    # llvm-cov: the first binary is positional, the others are --object.
    local report=("$cov" report --instr-profile "$CI/coverage/merged.profdata"
        --ignore-filename-regex '/test_rng\.rs$'
        "$target/debug/libfuturesdr_plugin_rt.so" "${objects[@]}")
    local sources=("$ROOT/api/src" "$ROOT/host/src" "$ROOT/sdk/src")
    "${report[@]}" "${sources[@]}" | tee "$CI/coverage/report.txt" |
        awk '{ sub(/.*crates\/plugin\//, ""); print }' | cut -c1-200
    "$cov" show --format=html --output-dir "$CI/coverage/html" \
        --instr-profile "$CI/coverage/merged.profdata" \
        "$target/debug/libfuturesdr_plugin_rt.so" "${objects[@]}" "${sources[@]}"
    echo "report: $CI/coverage/html/index.html"
    local lines
    lines=$(awk '/^TOTAL/ { print int($10) }' "$CI/coverage/report.txt")
    local min="${CI_MIN_COVERAGE:-90}"
    [ "$lines" -ge "$min" ] || fail "line coverage $lines% is below $min%"
    echo "line coverage $lines% (at least $min%)"
}

ALL=(fmt lint test plugins release sdk size core vendor miri stress coverage)
DEFAULT=(fmt lint test plugins sdk size)

if [ $# -eq 0 ]; then
    set -- "${DEFAULT[@]}"
elif [ "$1" = all ]; then
    set -- "${ALL[@]}"
fi
for stage in "$@"; do
    if ! declare -F "stage_$stage" >/dev/null; then
        fail "unknown stage '$stage' (stages: ${ALL[*]})"
    fi
done
for stage in "$@"; do
    "stage_$stage"
done
step "passed: $*"
