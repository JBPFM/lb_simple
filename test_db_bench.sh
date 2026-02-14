#!/bin/bash
set -e

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$SCRIPT_DIR"

default_lib_path() {
    local candidates=(
        "$PROJECT_DIR/target/debug/liblb_simple.so"
        "$PROJECT_DIR/target/release/liblb_simple.so"
    )
    local path
    for path in "${candidates[@]}"; do
        if [[ -f "$path" ]]; then
            printf "%s\n" "$path"
            return
        fi
    done
    printf "%s\n" "${candidates[0]}"
}

default_db_bench() {
    local candidates=(
        "$PROJECT_DIR/bench/flexguard/ext/leveldb-1.20/out-static/db_bench"
        "$PROJECT_DIR/bench/flexguard/ext/leveldb-1.20/out-shared/db_bench"
        "$PROJECT_DIR/bench/flexguard/ext/leveldb/build/db_bench"
    )
    local path
    for path in "${candidates[@]}"; do
        if [[ -x "$path" ]]; then
            printf "%s\n" "$path"
            return
        fi
    done
    printf "%s\n" "${candidates[0]}"
}

LIB_PATH="${LIB_PATH:-$(default_lib_path)}"
DB_BENCH="${DB_BENCH:-$(default_db_bench)}"

if [[ ! -f "$LIB_PATH" ]]; then
    echo "Error: $LIB_PATH not found. Run 'cargo build' first."
    exit 1
fi

if [[ ! -x "$DB_BENCH" ]]; then
    echo "Error: $DB_BENCH not found or not executable."
    echo "Hint: build LevelDB under bench/flexguard first."
    exit 1
fi

echo "=== Running db_bench with lb_simple scheduler ==="
echo "Pinned to CPU 0, 80 threads, 50000 operations"
echo ""

if [[ "$(id -u)" -eq 0 ]]; then
    SUDO=()
else
    SUDO=(sudo)
fi

"${SUDO[@]}" env LD_PRELOAD="$LIB_PATH" taskset -c 0 "$DB_BENCH" \
    --benchmarks=readrandom \
    --threads=80 \
    --num=50000
