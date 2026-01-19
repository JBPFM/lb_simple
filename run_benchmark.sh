if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
else
  SUDO="sudo"
fi

NUM="${1:-1000000}"

LB_SIMPLE_BIN="./target/release/lb_simple"
LB_SIMPLE_LIB="./target/release/liblb_simple.so"
DB_BENCH="${DB_BENCH:-$HOME/Projects/test/leveldb/build/db_bench}"

if [ -x "$LB_SIMPLE_BIN" ]; then
  $SUDO "$LB_SIMPLE_BIN" -- taskset -c 0 "$DB_BENCH" --benchmarks=readrandom --threads=80 --num="$NUM"
else
  $SUDO env LD_PRELOAD="$LB_SIMPLE_LIB" taskset -c 0 "$DB_BENCH" --benchmarks=readrandom --threads=80 --num="$NUM"
fi
