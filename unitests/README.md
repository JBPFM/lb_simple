## unitests

Build and run the pthread mutex counter test:

- `make`
- `./mutex_counter <threads_t> <loops_n> [hold_iters]`

Example:

- `./mutex_counter 8 100000`

To see contention more clearly, increase `loops_n`, and try `threads_t` larger than CPU cores:

- `./mutex_counter 1 20000000`
- `./mutex_counter 2 20000000`
- `./mutex_counter 4 20000000`
- `./mutex_counter 8 20000000`

If the difference is still not obvious, add `hold_iters` to extend time spent inside the mutex:

- `./mutex_counter 1 2000000 100`
- `./mutex_counter 8 2000000 100`

The program prints an `OK` line and a `STATS` line containing:

- `avg_op_ns`: average wall-clock nanoseconds per loop iteration (1 iteration = lock + increment + unlock)
- `qps`: operations per second (`ops / elapsed_s`)

Example output:

- `OK: counter=800000 expected=800000`
- `STATS: threads=8 loops=100000 ops=800000 elapsed_s=0.123456 avg_op_ns=154.32 qps=6481234.56`
