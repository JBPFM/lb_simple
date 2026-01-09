if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
else
  SUDO="sudo"
fi

$SUDO ./target/release/lb_simple --concurrency-mode "${LB_SIMPLE_CONCURRENCY_MODE:-default}" -- ~/test/test/leveldb/build/db_bench --benchmarks=readrandom --threads=80 --num=100000
