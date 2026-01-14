#!/bin/bash
#
# run_hypothesis_verification.sh
#
# 运行假设验证脚本的辅助工具
# 自动启动测试进程并绑定 bpftrace 采集
#
# 用法:
#   ./run_hypothesis_verification.sh <script> <mode> [options]
#
# 脚本:
#   combined    - 运行综合脚本（推荐）
#   h1          - 仅运行假设1验证
#   h2          - 仅运行假设2验证
#   h3          - 仅运行假设3验证
#   perf        - 运行 perf stat 采集缓存指标
#
# 模式:
#   baseline    - 使用 CFS 调度器（直接运行 db_bench）
#   lb_simple   - 使用 lb_simple 调度器

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="${SCRIPT_DIR}/hypothesis_results"

# 加载配置
source "$SCRIPT_DIR/config.sh"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

usage() {
    cat << EOF
用法: $0 <script> <mode> [options]

脚本类型:
  combined     运行综合假设验证脚本（推荐）
  h1           运行假设1验证：同时竞争者数量
  h2           运行假设2验证：CPU迁移和缓存效应
  h3           运行假设3验证：wakeup延迟和wake/wait循环
  perf         运行 perf stat 采集硬件计数器

运行模式:
  baseline     使用 CFS 调度器（直接运行测试程序）
  lb_simple    使用 lb_simple 调度器

选项:
  -d, --db-bench PATH    db_bench 路径 (默认: $DB_BENCH)
  -t, --threads NUM      线程数 (默认: 80)
  -n, --num NUM          操作数 (默认: $OPS)
  -b, --benchmark NAME   benchmark 名称 (默认: $BENCHMARK)
  -o, --output FILE      输出文件前缀
  -h, --help             显示帮助

示例:
  # 使用 CFS 运行综合脚本
  sudo $0 combined baseline -t 80

  # 使用 lb_simple 运行综合脚本
  sudo $0 combined lb_simple -t 80

  # 运行特定假设验证
  sudo $0 h1 baseline -t 128

  # 采集 perf 数据（自动后台运行 db_bench）
  sudo $0 perf lb_simple -t 80 -n 5000000
EOF
}

check_root() {
    if [[ $EUID -ne 0 ]]; then
        echo -e "${RED}错误: 需要 root 权限运行 bpftrace${NC}"
        echo "请使用: sudo $0 $@"
        exit 1
    fi
}

check_deps() {
    if [ ! -f "$DB_BENCH" ]; then
        echo -e "${RED}错误: db_bench 未找到: $DB_BENCH${NC}"
        echo "请通过 -d 选项指定或修改 config.sh"
        exit 1
    fi

    if ! command -v bpftrace &> /dev/null; then
        echo -e "${RED}错误: bpftrace 未安装${NC}"
        exit 1
    fi
}

# 解析参数
SCRIPT_TYPE="${1:-}"
MODE="${2:-}"
THREADS=80
OUTPUT_PREFIX=""

shift 2 2>/dev/null || true

while [[ $# -gt 0 ]]; do
    case $1 in
        -d|--db-bench)
            DB_BENCH="$2"
            shift 2
            ;;
        -t|--threads)
            THREADS="$2"
            shift 2
            ;;
        -n|--num)
            OPS="$2"
            shift 2
            ;;
        -b|--benchmark)
            BENCHMARK="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT_PREFIX="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo -e "${RED}未知选项: $1${NC}"
            usage
            exit 1
            ;;
    esac
done

# 验证参数
if [[ -z "$SCRIPT_TYPE" ]] || [[ -z "$MODE" ]]; then
    usage
    exit 1
fi

if [[ "$MODE" != "baseline" ]] && [[ "$MODE" != "lb_simple" ]]; then
    echo -e "${RED}错误: 模式必须是 baseline 或 lb_simple${NC}"
    usage
    exit 1
fi

check_root
check_deps

# 创建输出目录
mkdir -p "$OUTPUT_DIR"

# 生成输出文件名
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_BASE="${OUTPUT_DIR}/${MODE}_${SCRIPT_TYPE}_t${THREADS}_${TIMESTAMP}"

# 构建测试命令
DB_BENCH_CMD="$DB_BENCH --benchmarks=$BENCHMARK --threads=$THREADS --num=$OPS"

if [[ "$MODE" == "lb_simple" ]]; then
    # 检查 lb_simple 库是否存在
    if [ -f "$PROJECT_ROOT/$LB_SIMPLE_LIB" ]; then
        LB_SIMPLE_LIB_PATH="$PROJECT_ROOT/$LB_SIMPLE_LIB"
    elif [ -f "$LB_SIMPLE_LIB" ]; then
        LB_SIMPLE_LIB_PATH="$LB_SIMPLE_LIB"
    else
        echo -e "${RED}错误: liblb_simple.so 未找到，请先编译: cargo build --release${NC}"
        exit 1
    fi
    FULL_CMD="env LD_PRELOAD=$LB_SIMPLE_LIB_PATH $DB_BENCH_CMD"
else
    FULL_CMD="$DB_BENCH_CMD"
fi

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}假设验证测试${NC}"
echo -e "${BLUE}========================================${NC}"
echo "脚本类型:  $SCRIPT_TYPE"
echo "运行模式:  $MODE"
echo "线程数:    $THREADS"
echo "操作数:    $OPS"
echo "Benchmark: $BENCHMARK"
echo "命令:      $FULL_CMD"
echo "输出:      $OUTPUT_BASE.*"
echo ""

run_bpftrace() {
    local bt_script=$1
    local output_file=$2

    echo -e "${GREEN}运行 bpftrace...${NC}"
    echo -e "${YELLOW}bpftrace -c '$FULL_CMD' $bt_script${NC}"
    echo ""

    if [[ -n "$output_file" ]]; then
        bpftrace -c "$FULL_CMD" "$bt_script" 2>&1 | tee "$output_file"
    else
        bpftrace -c "$FULL_CMD" "$bt_script"
    fi
}

run_perf_stat() {
    local output_file=$1

    echo -e "${GREEN}运行 perf stat...${NC}"
    echo ""

    # perf stat 需要先启动命令，使用 --
    local perf_cmd="perf stat -e cache-misses,cache-references,LLC-load-misses,LLC-loads,cycles,instructions,context-switches,cpu-migrations -- $FULL_CMD"

    echo -e "${YELLOW}$perf_cmd${NC}"
    echo ""

    if [[ -n "$output_file" ]]; then
        echo "Command: $perf_cmd" > "$output_file"
        echo "---" >> "$output_file"
        eval "$perf_cmd" 2>&1 | tee -a "$output_file"
    else
        eval "$perf_cmd" 2>&1
    fi
}

case "$SCRIPT_TYPE" in
    combined)
        run_bpftrace "${SCRIPT_DIR}/hypothesis_all_combined.bt" "${OUTPUT_BASE}.txt"
        ;;
    h1)
        run_bpftrace "${SCRIPT_DIR}/hypothesis1_concurrent_competitors.bt" "${OUTPUT_BASE}.txt"
        ;;
    h2)
        run_bpftrace "${SCRIPT_DIR}/hypothesis2_cache_migration.bt" "${OUTPUT_BASE}.txt"
        ;;
    h3)
        run_bpftrace "${SCRIPT_DIR}/hypothesis3_wakeup_latency.bt" "${OUTPUT_BASE}.txt"
        ;;
    perf)
        run_perf_stat "${OUTPUT_BASE}.txt"
        ;;
    *)
        echo -e "${RED}未知脚本类型: $SCRIPT_TYPE${NC}"
        usage
        exit 1
        ;;
esac

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}完成！${NC}"
echo -e "${GREEN}========================================${NC}"
echo "结果保存到: ${OUTPUT_BASE}.txt"
echo ""
echo -e "${YELLOW}提示: 运行对比测试${NC}"
echo "  1. sudo $0 $SCRIPT_TYPE baseline -t $THREADS"
echo "  2. sudo $0 $SCRIPT_TYPE lb_simple -t $THREADS"
echo "  3. diff ${OUTPUT_DIR}/baseline_*.txt ${OUTPUT_DIR}/lb_simple_*.txt"
