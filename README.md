# lb_simple

`lb_simple` 是一个基于 `sched_ext + eBPF` 的 `LD_PRELOAD` 动态库。  
当前版本重点实现了“调度器协作锁（scheduler-cooperative lock）”：用户态互斥锁竞争与解锁让渡会主动向 BPF 调度器上报信息，调度器再按锁维度进行快速路由与 handoff。

## 当前功能

- `LD_PRELOAD` hook：
  - `pthread_mutex_init/destroy/lock/trylock/unlock`
  - `pthread_cond_init/destroy/wait/timedwait/signal/broadcast`
- 用户态锁实现：
  - `SpinparkLock`（TTAS + 自旋 + `sched_yield`）
  - 锁状态机：`UNLOCKED(0) / LOCKED(1) / QUEUED(2)`
  - 每把锁可绑定一个 VIP DSQ（最多 4096 个）
- 调度器协作路径（`src/mutex_hook.rs` + `src/bpf/main.bpf.c`）：
  - 竞争线程上报 `YIELD_LOCK_CONTENTION` + `vip_dsq_id`
  - `lb_simple_enqueue()` 将该线程放入对应 VIP DSQ
  - 解锁线程在有等待者时上报 `YIELD_LOCK_HANDOFF`
  - `lb_simple_yield()` / `lb_simple_dispatch()` 尝试把对应 VIP DSQ 任务快速拉到本地 CPU 运行队列
- 退出统计：
  - `lock_acquire`
  - `contention_yield`
  - `requeue_yield`
  - `handoff_yield`
  - `fallback_yield`
  - `handoff_taken`
  - `handoff_miss`

## 核心文件

- `src/mutex_hook.rs`：pthread hook、用户态锁/条件变量、TLS yield 信息上报
- `src/bpf/main.bpf.c`：sched_ext `yield/enqueue/dispatch` 与 VIP DSQ 路由
- `src/bpf/intf.h`：用户态与 BPF 共享常量和结构体
- `src/lib.rs`：动态库加载时初始化/附加调度器，退出时输出统计

## 系统要求

- Linux 内核支持 `sched_ext`（建议 6.6+）
- Rust 工具链（edition 2024）
- libbpf 开发环境
- root 权限（加载 eBPF 程序需要）

## 构建

```bash
cargo build --release
```

输出动态库：`target/release/liblb_simple.so`

## 使用方法

该项目当前是 `cdylib` 形态，不是独立命令行可执行程序。  
使用方式是把目标进程放在 `LD_PRELOAD` 下运行：

```bash
sudo LD_PRELOAD="$PWD/target/release/liblb_simple.so" <命令> [参数...]
```

示例：

```bash
sudo LD_PRELOAD="$PWD/target/release/liblb_simple.so" taskset -c 0 \
  ~/test/test/leveldb/build/db_bench \
  --benchmarks=readrandom --threads=80 --num=50000
```

## 协作锁流程（简版）

1. 线程尝试 `pthread_mutex_lock`，若竞争失败并达到 park 条件：
   - 写入 TLS `task_yield_info { reason=YIELD_LOCK_CONTENTION, vip_dsq_id }`
   - 调用 `sched_yield()`
2. BPF `lb_simple_yield()` 读取线程上报信息，记录到 `yield_addr_map`
3. 线程再次入队时，`lb_simple_enqueue()` 将其导入对应 VIP DSQ
4. 持锁线程 `pthread_mutex_unlock` 发现有等待者：
   - 设置 handoff 标记
   - 上报 `YIELD_LOCK_HANDOFF` 并 `sched_yield()`
5. BPF 优先 `scx_bpf_dsq_move_to_local(vip_dsq)`，失败则通过 per-cpu hint 在 `dispatch` 重试

## 限制与注意事项

- 仅实现了当前项目内的自定义 pthread hook 语义，不等价于 glibc futex 路径
- `pthread_mutexattr_t` / `pthread_condattr_t` 目前未使用（传入后被忽略）
- VIP DSQ 槽位上限为 4096；超出后走 `sched_yield` fallback 路径
- 若线程未成功注册 `yield_addr_map`，协作能力会退化为普通让出 CPU

## 开发环境辅助脚本

### `gen-compile-commands.sh`

生成 `compile_commands.json`，用于 IDE/clangd 理解 BPF C 编译参数。

```bash
./gen-compile-commands.sh
```

### `update-clangd.sh`

更新 `.clangd`，补齐 BPF 目标和头文件路径配置。

```bash
./update-clangd.sh
```

首次使用以上脚本前，先执行一次 `cargo build`。
