/* SPDX-License-Identifier: GPL-2.0-only */
#include <scx/common.bpf.h>
#include "intf.h"

char _license[] SEC("license") = "GPL";

const volatile pid_t pid_filter;

UEI_DEFINE(uei);

#define SHARED_DSQ 0
#define HELD_DSQ 1

#define SLICE_BOOST_NS (SCX_SLICE_DFL * 2)
#define NEAR_EXHAUST_THRESHOLD_NS 200000

struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);
  __type(value, struct held_lock_info);
  __uint(max_entries, 10000);
} held_locks SEC(".maps");

struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);
  __type(value, struct slice_track_info);
  __uint(max_entries, 10000);
} slice_track SEC(".maps");

struct {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, u32);
  __type(value, u64);
  __uint(max_entries, STAT_NR);
} stats SEC(".maps");

static void stat_inc(u32 idx) {
  u64 *cnt_p = bpf_map_lookup_elem(&stats, &idx);
  if (cnt_p)
    __sync_fetch_and_add(cnt_p, 1);
}

static inline bool is_holding_lock(u32 tid) {
  return bpf_map_lookup_elem(&held_locks, &tid) != NULL;
}

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle) {
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);
  }
  return cpu;
}

void BPF_STRUCT_OPS(lb_simple_enqueue, struct task_struct *p, u64 enq_flags) {
  u32 tid = p->pid;
  u64 slice_ns = SCX_SLICE_DFL;
  u64 dsq_id = SHARED_DSQ;

  if (is_holding_lock(tid)) {
    dsq_id = HELD_DSQ;
    slice_ns = SLICE_BOOST_NS;
    stat_inc(STAT_BOOST_APPLIED);
  }

  struct slice_track_info st = {
    .slice_budget_ns = slice_ns,
    .slice_start_ns = bpf_ktime_get_ns(),
    .near_exhaust = 0,
  };
  bpf_map_update_elem(&slice_track, &tid, &st, BPF_ANY);

  scx_bpf_dsq_insert(p, dsq_id, slice_ns, enq_flags);
}

void BPF_STRUCT_OPS(lb_simple_dispatch, s32 cpu, struct task_struct *prev) {
  if (scx_bpf_dsq_move_to_local(HELD_DSQ))
    return;
  scx_bpf_dsq_move_to_local(SHARED_DSQ);
}

void BPF_STRUCT_OPS(lb_simple_running, struct task_struct *p) {
  u32 tid = p->pid;
  struct slice_track_info *st = bpf_map_lookup_elem(&slice_track, &tid);
  if (st) {
    st->slice_start_ns = bpf_ktime_get_ns();
    st->near_exhaust = 0;
  }
}

void BPF_STRUCT_OPS(lb_simple_stopping, struct task_struct *p, bool runnable) {
  u32 tid = p->pid;

  if (!runnable) {
    bpf_map_delete_elem(&slice_track, &tid);
    return;
  }

  if (!is_holding_lock(tid))
    return;

  stat_inc(STAT_HOLD_SWITCHOUT_TOTAL);

  struct slice_track_info *st = bpf_map_lookup_elem(&slice_track, &tid);
  if (st && st->near_exhaust) {
    stat_inc(STAT_HOLD_SWITCHOUT_SLICE);
  } else {
    stat_inc(STAT_HOLD_SWITCHOUT_OTHER);
  }

  bpf_map_delete_elem(&slice_track, &tid);
}

void BPF_STRUCT_OPS(lb_simple_tick, struct task_struct *curr) {
  u32 tid = curr->pid;
  struct slice_track_info *st = bpf_map_lookup_elem(&slice_track, &tid);
  if (!st)
    return;

  u64 now = bpf_ktime_get_ns();
  u64 elapsed = now - st->slice_start_ns;

  if (st->slice_budget_ns > elapsed &&
      st->slice_budget_ns - elapsed <= NEAR_EXHAUST_THRESHOLD_NS) {
    st->near_exhaust = 1;
  }
}

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
  s32 ret;
  ret = scx_bpf_create_dsq(SHARED_DSQ, -1);
  if (ret)
    return ret;
  ret = scx_bpf_create_dsq(HELD_DSQ, -1);
  return ret;
}

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

SEC("tp/syscalls/sys_enter_futex")
int trace_futex(struct trace_event_raw_sys_enter *ctx) {
  u64 pid_tgid = bpf_get_current_pid_tgid();
  u32 pid = pid_tgid >> 32;
  u32 op;

  if (pid_filter != 0 && pid != pid_filter)
    return 0;

  bpf_probe_read_kernel(&op, sizeof(op), &ctx->args[1]);
  u32 futex_op = op & 0x7f;

  if (futex_op == 0) {
    stat_inc(STAT_FUTEX_WAIT);
  } else if (futex_op == 1) {
    stat_inc(STAT_FUTEX_WAKE);
  }

  return 0;
}

SCX_OPS_DEFINE(lb_simple_ops,
               .select_cpu = (void *)lb_simple_select_cpu,
               .enqueue = (void *)lb_simple_enqueue,
               .dispatch = (void *)lb_simple_dispatch,
               .running = (void *)lb_simple_running,
               .stopping = (void *)lb_simple_stopping,
               .tick = (void *)lb_simple_tick,
               .init = (void *)lb_simple_init,
               .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
