#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

source "$SCRIPT_DIR/config.sh"

FUTEX_BT="$SCRIPT_DIR/futex.bt"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info()  { echo -e "${GREEN}$*${NC}"; }
log_warn()  { echo -e "${YELLOW}$*${NC}"; }
log_error() { echo -e "${RED}$*${NC}" >&2; }

usage() {
    cat << EOF
Usage: $0 [options]

Options:
  -m, --mode MODE       Test mode: baseline or lb_simple (default: baseline)
  -d, --db-bench PATH   Path to db_bench binary
  -t, --threads LIST    Space-separated thread counts (default: from config.sh)
  -n, --num NUM         Operations per benchmark (default: $OPS)
  -b, --benchmark NAME  Benchmark name (default: $BENCHMARK)
  -r, --runs NUM        Runs per thread count (default: $RUNS)
  -o, --output DIR      Output directory (default: $OUTPUT_DIR)
  -h, --help            Show this help

Examples:
  $0 -m baseline -d /path/to/db_bench
  $0 -m lb_simple -d /path/to/db_bench -t "16 32 64"
  $0 -m baseline -n 50000 -r 5
EOF
}

MODE="baseline"
POSITIONAL_ARGS=()

while [[ $# -gt 0 ]]; do
    case $1 in
        -m|--mode)
            MODE="$2"
            shift 2
            ;;
        -d|--db-bench)
            DB_BENCH="$2"
            shift 2
            ;;
        -t|--threads)
            THREAD_COUNTS="$2"
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
        -r|--runs)
            RUNS="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            usage
            exit 1
            ;;
    esac
done

if [ ! -f "$DB_BENCH" ]; then
    log_error "db_bench not found: $DB_BENCH"
    log_error "Please set DB_BENCH in config.sh or use -d/--db-bench option"
    exit 1
fi

if [ ! -f "$FUTEX_BT" ]; then
    log_error "futex.bt not found: $FUTEX_BT"
    exit 1
fi

if [ "$MODE" = "lb_simple" ]; then
    # Prefer the new usage: `sudo lb_simple -- <cmd>`
    if [ -x "$PROJECT_ROOT/$LB_SIMPLE_BIN" ]; then
        LB_SIMPLE_BIN_PATH="$PROJECT_ROOT/$LB_SIMPLE_BIN"
        LB_SIMPLE_METHOD="bin"
    elif [ -x "$LB_SIMPLE_BIN" ]; then
        LB_SIMPLE_BIN_PATH="$LB_SIMPLE_BIN"
        LB_SIMPLE_METHOD="bin"
    elif [ -f "$PROJECT_ROOT/$LB_SIMPLE_LIB" ]; then
        LB_SIMPLE_LIB_PATH="$PROJECT_ROOT/$LB_SIMPLE_LIB"
        LB_SIMPLE_METHOD="preload"
    elif [ -f "$LB_SIMPLE_LIB" ]; then
        LB_SIMPLE_LIB_PATH="$LB_SIMPLE_LIB"
        LB_SIMPLE_METHOD="preload"
    else
        log_error "lb_simple not found."
        log_error "Tried executable: $LB_SIMPLE_BIN and library: $LB_SIMPLE_LIB"
        log_error "Please build first: cargo build --release"
        exit 1
    fi

    log_info "lb_simple method: $LB_SIMPLE_METHOD"
fi

FULL_OUTPUT_DIR="$SCRIPT_DIR/$OUTPUT_DIR"
mkdir -p "$FULL_OUTPUT_DIR"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)

log_info "Lock Overhead Test"
echo "===================="
echo "Mode:        $MODE"
echo "DB_BENCH:    $DB_BENCH"
echo "Benchmark:   $BENCHMARK"
echo "Operations:  $OPS"
echo "Runs:        $RUNS"
echo "Threads:     $THREAD_COUNTS"
echo "Output:      $FULL_OUTPUT_DIR"
echo ""

run_single_test() {
    local threads="$1"
    local run_num="$2"
    local output_file="$3"
    
    local db_bench_cmd="$DB_BENCH --benchmarks=$BENCHMARK --threads=$threads --num=$OPS"

    if [ "$MODE" = "baseline" ]; then
        sudo taskset -c 0 bpftrace "$FUTEX_BT" -c "$db_bench_cmd" 2>&1
    elif [ "$MODE" = "lb_simple" ]; then
        if [ "${LB_SIMPLE_METHOD:-}" = "bin" ]; then
            sudo bpftrace "$FUTEX_BT" -c "$LB_SIMPLE_BIN_PATH -- $db_bench_cmd" 2>&1
        else
            sudo taskset -c 0 bpftrace "$FUTEX_BT" -c "env LD_PRELOAD=$LB_SIMPLE_LIB_PATH $db_bench_cmd" 2>&1
        fi
    else
        log_error "Unknown mode: $MODE"
        return 1
    fi
}

for threads in $THREAD_COUNTS; do
    log_warn "Testing with $threads thread(s)..."
    
    for run in $(seq 1 $RUNS); do
        echo "  Run $run/$RUNS..."
        
        OUTPUT_FILE="$FULL_OUTPUT_DIR/${MODE}_${BENCHMARK}_t${threads}_r${run}_${TIMESTAMP}.txt"
        
        if ! run_single_test "$threads" "$run" > "$OUTPUT_FILE" 2>&1; then
            log_error "  Warning: Run $run failed, see $OUTPUT_FILE for details"
            continue
        fi
        
        echo "  Saved: $OUTPUT_FILE"
        sleep 1
    done
done

log_info "All tests completed!"
echo "Results saved to: $FULL_OUTPUT_DIR/"
