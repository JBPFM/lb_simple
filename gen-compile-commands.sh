#!/bin/bash
# Generate compile_commands.json for BPF code

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
SCX_UTILS_VERSION=$(grep 'scx_utils.*version' Cargo.toml | head -1 | sed 's/.*version.*=.*"\([^"]*\)".*/\1/')
SCX_UTILS_PATH="$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/scx_utils-${SCX_UTILS_VERSION}/bpf_h"

cat > compile_commands.json <<EOF
[
  {
    "directory": "$SCRIPT_DIR",
    "command": "clang --target=bpf -I/usr/local/include -I/usr/include -I/usr/include/x86_64-linux-gnu -I$SCX_UTILS_PATH -I$BUILD_OUT_DIR -D__BPF__ -D__BPF_TRACING__ -D__TARGET_ARCH_x86 -Wno-unknown-attributes -Wno-visibility -Wno-address-of-packed-member -Wno-compare-distinct-pointer-types -Wno-gnu-variable-sized-type-not-at-end -Wno-pointer-sign -Wno-pragma-once-outside-header -Wno-unused-value -c src/bpf/main.bpf.c",
    "file": "src/bpf/main.bpf.c"
  }
]
EOF

echo "Successfully generated compile_commands.json"
