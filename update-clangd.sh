#!/bin/bash
# Script to update .clangd configuration with the latest build directory

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Find the most recent build output directory
LATEST_BUILD=$(find target/debug/build -name "lb_simple-*" -type d -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2)

if [ -z "$LATEST_BUILD" ]; then
    echo "Error: No build directory found. Please run 'cargo build' first."
    exit 1
fi

BUILD_OUT_DIR="$LATEST_BUILD/out/scx_utils-bpf_h"

if [ ! -d "$BUILD_OUT_DIR" ]; then
    echo "Error: Build output directory not found: $BUILD_OUT_DIR"
    exit 1
fi

echo "Found build directory: $BUILD_OUT_DIR"

# Get scx_utils version from Cargo.toml
SCX_UTILS_VERSION=$(grep 'scx_utils.*version' Cargo.toml | head -1 | sed 's/.*version.*=.*"\([^"]*\)".*/\1/')
SCX_UTILS_PATH="$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/scx_utils-${SCX_UTILS_VERSION}/bpf_h"

cat > .clangd <<EOF
CompileFlags:
  Add:
    # BPF target architecture
    - --target=bpf

    # Include paths for BPF headers
    - -I/usr/local/include
    - -I/usr/include
    - -I/usr/include/x86_64-linux-gnu

    # Include scx headers from cargo registry
    - -I$SCX_UTILS_PATH

    # Include build output directory (contains vmlinux.h)
    - -I$BUILD_OUT_DIR

    # BPF-specific defines
    - -D__BPF__
    - -D__BPF_TRACING__
    - -D__TARGET_ARCH_x86

    # Disable some warnings that are common in BPF code
    - -Wno-unknown-attributes
    - -Wno-visibility
    - -Wno-address-of-packed-member
    - -Wno-compare-distinct-pointer-types
    - -Wno-gnu-variable-sized-type-not-at-end
    - -Wno-pointer-sign
    - -Wno-pragma-once-outside-header
    - -Wno-unused-value

  Remove:
    # Remove flags that might conflict with BPF compilation
    - -msse*
    - -march*

Diagnostics:
  # Suppress some diagnostics that are not relevant for BPF
  Suppress:
    - pp_file_not_found
  ClangTidy:
    Remove:
      - bugprone-*
      - modernize-*
      - readability-*
      - google-*
      - cppcoreguidelines-*

Index:
  Background: Build
EOF

echo "Successfully updated .clangd configuration"
echo "Build output directory: $BUILD_OUT_DIR"
