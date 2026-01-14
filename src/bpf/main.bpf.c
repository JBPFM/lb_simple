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
#include "intf.h"

char _license[] SEC("license") = "GPL";

const volatile unsigned long long tick_interval_ns;
const volatile unsigned long long tick_guard_ns;
const volatile unsigned long long tick_extra_ns;
const volatile unsigned long long max_boost_hold_ns;

UEI_DEFINE(uei);

/* Map 容量常量 */
#define THREAD_STATE_MAP_MAX_ENTRIES 100000
#define CS_START_MAP_MAX_ENTRIES     100000

/* 默认 tick 间隔 (1ms = 1000000ns) */
#define DEFAULT_TICK_INTERVAL_NS     1000000ULL

/* thread_state_ptrs: tid -> 用户态 TLS 指针，用于读取锁深度 */
struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);
  __type(value, unsigned long long);
  __uint(max_entries, THREAD_STATE_MAP_MAX_ENTRIES);
} thread_state_ptrs SEC(".maps");

/* cs_start_ns: tid -> 临界区开始时间 (ns)，用于限制最大续期时间 */
struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);
  __type(value, unsigned long long);
  __uint(max_entries, CS_START_MAP_MAX_ENTRIES);
} cs_start_ns SEC(".maps");

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle) {
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);
  }
  return cpu;
}

static __always_inline bool read_user_lock_depth(u32 tid, unsigned int *out_depth)
{
  unsigned long long *uptr;

  uptr = bpf_map_lookup_elem(&thread_state_ptrs, &tid);
  if (!uptr)
    return false;

  if (bpf_probe_read_user(out_depth, sizeof(*out_depth), (void *)*uptr))
    return false;

  return true;
}

/*
 * track_cs_duration - 追踪临界区持续时间
 * @tid: 线程 ID
 * @now: 当前时间戳 (ns)
 *
 * 返回值:
 *   true  - 临界区有效，可以续期
 *   false - 超过最大续期时间，不再续期
 */
static __always_inline bool track_cs_duration(u32 tid, unsigned long long now)
{
  unsigned long long *startp;

  startp = bpf_map_lookup_elem(&cs_start_ns, &tid);
  if (!startp) {
    bpf_map_update_elem(&cs_start_ns, &tid, &now, BPF_ANY);
    return true;
  }

  if (max_boost_hold_ns && now - *startp > max_boost_hold_ns)
    return false;

  return true;
}

/*
 * try_extend_slice - 尝试续期时间片
 * @p: 任务结构体
 *
 * 仅当时间片即将耗尽时才续期，避免过度续期导致其他任务饥饿
 */
static __always_inline void try_extend_slice(struct task_struct *p)
{
  unsigned long long interval, threshold, target;

  interval = tick_interval_ns ? tick_interval_ns : DEFAULT_TICK_INTERVAL_NS;
  threshold = interval + tick_guard_ns;

  if (p->scx.slice > threshold)
    return;

  target = interval + tick_extra_ns;
  if (p->scx.slice < target)
    p->scx.slice = target;
}

void BPF_STRUCT_OPS(lb_simple_tick, struct task_struct *p)
{
  unsigned int depth;
  unsigned long long now;
  u32 tid;

  tid = p->pid;
  if (!read_user_lock_depth(tid, &depth))
    return;

  now = bpf_ktime_get_ns();

  if (!depth) {
    bpf_map_delete_elem(&cs_start_ns, &tid);
    return;
  }

  if (!track_cs_duration(tid, now))
    return;

  try_extend_slice(p);
}

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
  return 0;
}

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

SCX_OPS_DEFINE(lb_simple_ops,
               .select_cpu = (void *)lb_simple_select_cpu,
               .tick = (void *)lb_simple_tick,
               .init = (void *)lb_simple_init,
               .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
