# add_one_dyn Example

Dynamic flowgraph example: **NullSource → [optional: AddOne] → Head(1) → FileSink**

## Quick Start

### 1. First Time Setup
Build the FutureSDR core and required plugins (run once):
```bash
bash setup.sh
```

### 2. Compile the Example
Build the add_one plugin and example application:
```bash
bash compile.sh
```

This script:
- Cleans previous builds (for repeatability)
- Builds the add_one plugin
- Builds the add_one_dyn example binary
- Collects all shared libraries to `shared_libs_debug/`

### 3. Run Tests
Test both configurations (with and without add_one):
```bash
bash run_tests.sh
```

## What It Does

### Without --add-one flag
- **Flow**: NullSource (outputs 0) → Head(1) → FileSink
- **Expected output**: First byte is `0x00`
- **File**: `/tmp/output_without_add_one.bin`

### With --add-one flag
- **Flow**: NullSource (outputs 0) → AddOne (+1) → Head(1) → FileSink
- **Expected output**: First byte is `0x01`
- **File**: `/tmp/output_with_add_one.bin`

## File Structure

```
add_one_dyn/
├── add_one_plugin/              # Local add_one plugin
│   ├── Cargo.toml
│   └── src/lib.rs
├── flows/                       # Flowgraph TOML configs
│   ├── fg_with_add_one.toml
│   └── fg_without_add_one.toml
├── src/
│   └── main.rs                  # Main binary
├── Cargo.toml                   # Example Cargo config
├── setup.sh                     # One-time setup (builds core)
├── compile.sh                   # Compile plugin & example
├── run_tests.sh                 # Test both configurations
└── README.md                    # This file
```

## How It Works

1. **setup.sh**: Builds FutureSDR core and required plugins once
2. **compile.sh**: 
   - Cleans previous plugin/example builds
   - Rebuilds add_one plugin with same ABI as core
   - Rebuilds example binary
   - Collects all .so files to `shared_libs_debug/`
3. **run_tests.sh**:
   - Runs example without --add-one, verifies output is 0
   - Runs example with --add-one, verifies output is 1

## Plugin Discovery

The binary uses `default_plugin_dir()` from plugin_api, which searches in:
1. `PLUGIN_DIR` environment variable (if set)
2. Directory of the running executable
3. Current directory

The test script sets `PLUGIN_DIR` to include `shared_libs_debug/` for plugin discovery.

## Troubleshooting

**Plugins not found?**
- Check that setup.sh was run: `ls shared_libs_debug/`
- Ensure compile.sh completed successfully

**File tests fail?**
- Check `/tmp/output_*.bin` exist and have correct size
- Verify plugin loading by checking run_tests.sh output for errors

**ABI mismatch errors?**
- Make sure setup.sh was run before compile.sh
- All builds use the same Rust toolchain (see rust-toolchain.toml)
