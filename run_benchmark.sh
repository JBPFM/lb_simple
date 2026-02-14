#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <num>" >&2
  exit 1
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$SCRIPT_DIR"

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

LIB_PATH="${LIB_PATH:-$PROJECT_DIR/target/release/liblb_simple.so}"
DB_BENCH="${DB_BENCH:-$(default_db_bench)}"

if [[ ! -f "$LIB_PATH" ]]; then
  echo "Error: $LIB_PATH not found. Run 'cargo build --release' first." >&2
  exit 1
fi

if [[ ! -x "$DB_BENCH" ]]; then
  echo "Error: db_bench not found or not executable: $DB_BENCH" >&2
  echo "Hint: build LevelDB under bench/flexguard first." >&2
  exit 1
fi

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=()
else
  SUDO=(sudo)
fi

"${SUDO[@]}" env LD_PRELOAD="$LIB_PATH" taskset -c 0 "$DB_BENCH" --benchmarks=readrandom --threads=80 --num="$1"
