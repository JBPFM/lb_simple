if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
else
  SUDO="sudo"
fi

$SUDO LD_PRELOAD="./target/release/liblb_simple.so" taskset -c 0 ~/test/test/leveldb/build/db_bench --benchmarks=readrandom --threads=80 --num=$1
