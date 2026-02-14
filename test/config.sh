#!/bin/bash
# 锁开销测试配置文件
# 可通过环境变量或直接修改此文件来覆盖默认值

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

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

# db_bench 路径（默认指向 bench/flexguard 下的 leveldb 构建产物）
DB_BENCH="${DB_BENCH:-$(default_db_bench)}"

# lb_simple 库路径 (用于 LD_PRELOAD)
LB_SIMPLE_LIB="${LB_SIMPLE_LIB:-./target/release/liblb_simple.so}"

# 线程数列表
THREAD_COUNTS="${THREAD_COUNTS:-16 32 48 64 80 96 112 128 160 192 256}"

# 每个 benchmark 的操作数
OPS="${OPS:-1000000}"

# benchmark 名称
BENCHMARK="${BENCHMARK:-readrandom}"

# 每个线程数运行次数
RUNS="${RUNS:-3}"

# 输出目录
OUTPUT_DIR="${OUTPUT_DIR:-results}"
