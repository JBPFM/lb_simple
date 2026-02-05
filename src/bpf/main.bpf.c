/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * lb_simple - 基于 sched_ext 的锁感知调度器
 *
 * 核心功能：检测线程是否持有用户态锁，若持有则在时间片即将耗尽时自动续期，
 * 防止持有锁的线程被切出导致优先级反转或锁竞争加剧。
 *
 * 工作原理：
 * 1. 用户态通过 LD_PRELOAD 钩住 pthread_mutex_* 函数，维护 TLS 中的锁深度
 * 2. 线程创建时将 TLS 地址注册到 thread_state_ptrs map
 * 3. BPF tick 回调通过 bpf_probe_read_user 读取 TLS 状态
 * 4. 若线程在临界区内且时间片即将耗尽，则续期时间片
 */
#include <scx/common.bpf.h>

// #define TICK_EXTEND
#define EAGER_ENQUEUE

char _license[] SEC("license") = "GPL";

const volatile unsigned long long tick_interval_ns;
const volatile unsigned long long tick_guard_ns;
const volatile unsigned long long tick_extra_ns;
const volatile unsigned long long max_boost_hold_ns;
const volatile u32 nr_cores = 0;

UEI_DEFINE(uei);

/* 自定义 DSQ ID 定义 */
#define CUSTOM_DSQ_GLOBAL 0
/* Per-CPU DSQ 从 1 开始编号: DSQ ID = 1 + cpu_id */
#define CUSTOM_DSQ_PERCPU_BASE 1

/* Map 容量常量 */
#define THREAD_STATE_MAP_MAX_ENTRIES 100000
#define CS_START_MAP_MAX_ENTRIES 100000

/* 默认 tick 间隔 (1ms = 1000000ns) */
#define DEFAULT_TICK_INTERVAL_NS 1000000ULL

/* thread_state_ptrs: tid -> 用户态 TLS 指针，用于读取锁深度 */
struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);
  __type(value, unsigned long long);
  __uint(max_entries, THREAD_STATE_MAP_MAX_ENTRIES);
} thread_state_ptrs SEC(".maps");

static __always_inline bool read_user_lock_depth(u32 tid,
                                                 unsigned int *out_depth) {
  unsigned long long *uptr;

  uptr = bpf_map_lookup_elem(&thread_state_ptrs, &tid);
  if (!uptr)
    return false;

  if (bpf_probe_read_user(out_depth, sizeof(*out_depth), (void *)*uptr))
    return false;

  return true;
}

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu;
  u64 dsq_id;

  cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle) {
    /* 使用自定义的 per-cpu 优先级队列 */
    dsq_id = CUSTOM_DSQ_PERCPU_BASE + cpu;
    scx_bpf_dsq_insert(p, dsq_id, SCX_SLICE_DFL, 0);
  }
  return cpu;
}

void BPF_STRUCT_OPS(lb_simple_enqueue, struct task_struct *p, u64 enq_flags) {
  s32 task_cpu;
  u64 dsq_id;
  u32 tid;
  unsigned int depth;

  #ifdef EAGER_ENQUEUE
  /*
   * 优先调度策略：持有锁的线程直接入队到 local 队列
   * 这样可以最快得到调度，减少临界区持有时间，降低锁竞争
   */
  tid = p->pid;
  if (read_user_lock_depth(tid, &depth) && depth > 0) {
    /* 持锁线程直接插入 local 队列，获得最高优先级 */
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, enq_flags | SCX_ENQ_HEAD | SCX_ENQ_PREEMPT);
    return;
  }
#endif

  /*
   * 未持锁线程：优先入队到任务的目标 CPU 的 per-cpu 队列
   * 如果无法确定 CPU，则入队到 global 队列
   *
   * 注：当 select_cpu 中调用 scx_bpf_dsq_insert 后，
   * enqueue 回调不会被调用，无需额外检查
   */
  task_cpu = scx_bpf_task_cpu(p);
  if (task_cpu >= 0 && (u32)task_cpu < nr_cores) {
    dsq_id = CUSTOM_DSQ_PERCPU_BASE + task_cpu;
  } else {
    dsq_id = CUSTOM_DSQ_GLOBAL;
  }

  scx_bpf_dsq_insert(p, dsq_id, SCX_SLICE_DFL, enq_flags);
}

void BPF_STRUCT_OPS(lb_simple_dispatch, s32 cpu, struct task_struct *prev) {
  u64 dsq_id;

  /* 首先从本 CPU 的 per-cpu 队列消费 */
  dsq_id = CUSTOM_DSQ_PERCPU_BASE + cpu;
  if (scx_bpf_dsq_move_to_local(dsq_id))
    return;

  /* 然后从 global 队列消费 */
  scx_bpf_dsq_move_to_local(CUSTOM_DSQ_GLOBAL);
}

/*
 * try_extend_slice - 尝试续期时间片
 * @p: 任务结构体
 *
 * 仅当时间片即将耗尽时才续期，避免过度续期导致其他任务饥饿
 */
static __always_inline void try_extend_slice(struct task_struct *p) {
  unsigned long long interval, threshold, target;

  interval = tick_interval_ns ? tick_interval_ns : DEFAULT_TICK_INTERVAL_NS;
  threshold = interval + tick_guard_ns;

  if (p->scx.slice > threshold)
    return;

  target = interval + tick_extra_ns;
  if (p->scx.slice < target)
    p->scx.slice = target;
}

#ifdef TICK_EXTEND
void BPF_STRUCT_OPS(lb_simple_tick, struct task_struct *p) {
  unsigned int depth;
  u32 tid;

  tid = p->pid;
  if (!read_user_lock_depth(tid, &depth))
    return;

  if (!depth) {
    return;
  }

  try_extend_slice(p);
}
#endif

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
  u32 i;
  s32 ret;

  /* 创建自定义 global 队列 */
  ret = scx_bpf_create_dsq(CUSTOM_DSQ_GLOBAL, -1);
  if (ret) {
    scx_bpf_error("创建 global DSQ 失败: %d", ret);
    return ret;
  }

  /*
   * 创建 nr_cores 个 per-cpu 优先级队列
   * 使用 -1 作为 NUMA node 参数，让内核自动选择最优节点
   * 避免跨 NUMA 节点访问延迟
   */
  for (i = 0; i < nr_cores; i++) {
    ret = scx_bpf_create_dsq(CUSTOM_DSQ_PERCPU_BASE + i, -1);
    if (ret) {
      scx_bpf_error("创建 per-cpu DSQ[%u] 失败: %d", i, ret);
      return ret;
    }
  }

  return 0;
}

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

SCX_OPS_DEFINE(lb_simple_ops,
               .select_cpu = (void *)lb_simple_select_cpu,
               .enqueue = (void *)lb_simple_enqueue,
               .dispatch = (void *)lb_simple_dispatch,
               #ifdef TICK_EXTEND
               .tick = (void *)lb_simple_tick,
               #endif
               .init = (void *)lb_simple_init,
               .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
