#!/bin/bash
#
# run_delayed_wakeup_test.sh - 运行延迟唤醒假设验证测试
#

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BT_SCRIPT="$SCRIPT_DIR/hypothesis_delayed_wakeup.bt"
RESULTS_DIR="$SCRIPT_DIR/hypothesis_results"
LB_SIMPLE_LIB="$PROJECT_DIR/target/release/liblb_simple.so"

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

DB_BENCH="${DB_BENCH:-$(default_db_bench)}"

THREADS=80
OPS=500000

mkdir -p "$RESULTS_DIR"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)

echo "=============================================="
echo "延迟唤醒假设验证测试"
echo "=============================================="
echo "线程数: $THREADS"
echo "操作数: $OPS"
echo ""

# 测试函数
run_test() {
    local mode=$1
    local output_file="$RESULTS_DIR/${mode}_delayed_wakeup_t${THREADS}_${TIMESTAMP}.txt"

    echo ">>> 运行 $mode 测试..."

    if [ "$mode" == "baseline" ]; then
        # Baseline: 直接运行 db_bench
        local cmd="$DB_BENCH --benchmarks=readrandom --threads=$THREADS --num=$OPS"
    else
        # lb_simple: 使用 LD_PRELOAD 加载库
        local cmd="env LD_PRELOAD=$LB_SIMPLE_LIB $DB_BENCH --benchmarks=readrandom --threads=$THREADS --num=$OPS"
    fi

    echo "命令: bpftrace -c '$cmd' $BT_SCRIPT"
    echo ""

    sudo bpftrace -c "$cmd" "$BT_SCRIPT" 2>&1 | tee "$output_file"

    echo ""
    echo ">>> $mode 测试完成，结果保存到: $output_file"
    echo ""
}

# 确保 lb_simple 库已编译
if [ ! -f "$LB_SIMPLE_LIB" ]; then
    echo "编译 liblb_simple.so..."
    cd "$PROJECT_DIR" && cargo build --release
fi

if [[ ! -x "$DB_BENCH" ]]; then
    echo "错误: db_bench 未找到或不可执行: $DB_BENCH"
    echo "请先在 bench/flexguard 下构建 leveldb。"
    exit 1
fi

# 运行测试
case "${1:-both}" in
    baseline)
        run_test baseline
        ;;
    lb_simple)
        run_test lb_simple
        ;;
    both|*)
        run_test baseline
        echo "=============================================="
        echo "等待 5 秒后运行 lb_simple 测试..."
        echo "=============================================="
        sleep 5
        run_test lb_simple
        ;;
esac

echo "=============================================="
echo "测试完成！"
echo "结果文件位于: $RESULTS_DIR"
echo "=============================================="
