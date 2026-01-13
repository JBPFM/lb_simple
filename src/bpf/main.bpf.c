/* SPDX-License-Identifier: GPL-2.0-only */
#include <scx/common.bpf.h>
#include "intf.h"

char _license[] SEC("license") = "GPL";

const volatile pid_t pid_filter;

UEI_DEFINE(uei);

#define SHARED_DSQ 0
#define HELD_DSQ 1

struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, u32);
  __type(value, struct held_lock_info);
  __uint(max_entries, 10000);
} held_locks SEC(".maps");
//
// struct {
//   __uint(type, BPF_MAP_TYPE_ARRAY);
//   __type(key, u32);
//   __type(value, u64);
//   __uint(max_entries, STAT_NR);
// } stats SEC(".maps");
//
// static inline bool is_holding_lock(u32 tid) {
//   return bpf_map_lookup_elem(&held_locks, &tid) != NULL;
// }

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle) {
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);
  }
  return cpu;
}

// void BPF_STRUCT_OPS(lb_simple_enqueue, struct task_struct *p, u64 enq_flags) {
//   scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, enq_flags);
// }

// void BPF_STRUCT_OPS(lb_simple_dispatch, s32 cpu, struct task_struct *prev) {
// }

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
  return 0;
}

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

SCX_OPS_DEFINE(lb_simple_ops,
               .select_cpu = (void *)lb_simple_select_cpu,
               // .enqueue = (void *)lb_simple_enqueue,
               // .dispatch = (void *)lb_simple_dispatch,
               .init = (void *)lb_simple_init,
               .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
