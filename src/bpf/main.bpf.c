/* SPDX-License-Identifier: GPL-2.0-only */
#include <scx/common.bpf.h>

char _license[] SEC("license") = "GPL";

const volatile bool use_cgroup_filter;

UEI_DEFINE(uei);

#define SHARED_DSQ 0

// lock_addr -> last_run_cpu
struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u64);   /* lock_addr */
  __type(value, u32); /* last_run_cpu */
  __uint(max_entries, 10000);
} lock_owners SEC(".maps");

// tid -> lock_addr
struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);   /* tid */
  __type(value, u64); /* lock_addr */
  __uint(max_entries, 10000);
} thread_stats SEC(".maps");

// 统计信息
struct {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, u32);
  __type(value, u64);
  __uint(max_entries, 2); /* [futex_wait_count, futex_wake_count] */
} stats SEC(".maps");

// 允许用户空间限制监测的 cgroup
struct {
  __uint(type, BPF_MAP_TYPE_CGROUP_ARRAY);
  __uint(max_entries, 1);
  __type(key, u32);
  __type(value, u32);
} cgroup_filter SEC(".maps");

static void stat_inc(u32 idx) {
  u64 *cnt_p = bpf_map_lookup_elem(&stats, &idx);
  if (cnt_p)
    (*cnt_p)++;
}

// 查询thread_stats中的lock_addr，返回指针以区分"未找到"和"值为0"
static inline u64 *get_lock_addr_ptr(u32 tid) {
  return bpf_map_lookup_elem(&thread_stats, &tid);
}

// 查询lock_owners中的last_run_cpu，返回指针以区分"未找到"和"值为0"
static inline u32 *get_last_run_cpu_ptr(u64 lock_addr) {
  return bpf_map_lookup_elem(&lock_owners, &lock_addr);
}

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu;
  u32 tid = p->pid;
  u64 *lock_addr_p;
  u32 *target_cpu_p;

  /* 查询当前线程是否持有锁 */
  lock_addr_p = get_lock_addr_ptr(tid);

  /* 删除 thread_stats 中当前 tid 的表项 */
  bpf_map_delete_elem(&thread_stats, &tid);

  if (lock_addr_p) {
    /* 线程持有锁，查询锁的 last_run_cpu */
    target_cpu_p = get_last_run_cpu_ptr(*lock_addr_p);
    if (target_cpu_p) {
      u32 target_cpu = *target_cpu_p;

      /* 验证 target_cpu 是否在允许的 CPU 集合中 */
      if (bpf_cpumask_test_cpu(target_cpu, p->cpus_ptr)) {
        /* 检查 CPU 是否空闲 */
        scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL_ON | target_cpu, SCX_SLICE_DFL, 0);
        return target_cpu;
        /* CPU 不空闲，但仍然倾向于使用它（通过返回它作为建议） */
        /* 任务会进入 enqueue，最终可能在该 CPU 上运行 */
      }
    }
  }

  /* 没有锁或无法使用 last_run_cpu，使用默认策略 */
  cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle) {
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);
  }

  return cpu;
}

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
  return scx_bpf_create_dsq(SHARED_DSQ, -1);
}

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

/* Tracepoint: sys_enter_futex - 捕获 futex wait 和 wake 操作 */
SEC("tp/syscalls/sys_enter_futex")
int trace_futex(struct trace_event_raw_sys_enter *ctx) {
  u64 pid_tgid = bpf_get_current_pid_tgid();
  u32 tid = (u32)pid_tgid;
  u32 op;
  u64 lock_addr;
  u32 idx = 0;

  if (use_cgroup_filter && !bpf_current_task_under_cgroup(&cgroup_filter, idx))
    return 0;

  /* 读取 futex 操作类型（第二个参数） */
  bpf_probe_read_kernel(&op, sizeof(op), &ctx->args[1]);

  /* 获取 futex 地址（锁地址） */
  lock_addr = ctx->args[0];

  /* 提取操作类型（低 7 位） */
  u32 futex_op = op & 0x7f;

  if (futex_op == 0) {
    /* FUTEX_WAIT: 记录 tid -> lock_addr 映射 */
    bpf_map_update_elem(&thread_stats, &tid, &lock_addr, BPF_ANY);
    stat_inc(0); /* futex_wait 计数 */
  } else if (futex_op == 1) {
    /* FUTEX_WAKE: 记录 lock_addr -> last_run_cpu 映射 */
    u32 cpu = bpf_get_smp_processor_id();
    bpf_map_update_elem(&lock_owners, &lock_addr, &cpu, BPF_ANY);
    stat_inc(1); /* futex_wake 计数 */
  }

  return 0;
}

SCX_OPS_DEFINE(lb_simple_ops, .select_cpu = (void *)lb_simple_select_cpu,
               .init = (void *)lb_simple_init, .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
