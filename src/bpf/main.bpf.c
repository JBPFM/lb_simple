/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * lb_simple - 基于 sched_ext 的锁感知调度器
 *
 * 核心功能：在 tick 周期内按阈值检查 slice，必要时进行小幅续期，
 * 减少频繁切出带来的调度抖动。
 *
 * 工作原理：
 * 1. BPF tick 回调检查任务当前剩余 slice
 * 2. 若本 tick 周期内可能耗尽，则把 slice 补到目标值
 */
#include <scx/common.bpf.h>

char _license[] SEC("license") = "GPL";

UEI_DEFINE(uei);

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle) {
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);
  }
  return cpu;
}

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) { return 0; }

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

SCX_OPS_DEFINE(lb_simple_ops, .select_cpu = (void *)lb_simple_select_cpu,
               .init = (void *)lb_simple_init, .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
