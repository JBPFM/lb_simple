/* SPDX-License-Identifier: GPL-2.0-only */
#include <scx/common.bpf.h>

char _license[] SEC("license") = "GPL";

const volatile pid_t pid_filter;
/*
 * 并发控制模式：
 * 0: 默认（完全使用 scx_bpf_select_cpu_dfl）
 * 1: NUMA 模式（仅允许调度到当前 CPU 所在 NUMA 节点）
 * 2: 严格模式（仅允许调度到上一次运行的 CPU，忽略其忙碌状态）
 */
const volatile u32 concurrency_mode;

UEI_DEFINE(uei);

#define SHARED_DSQ 0

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
  bool is_idle = false;
  s32 cpu;

  if (concurrency_mode == 2) {
    if (prev_cpu >= 0 && bpf_cpumask_test_cpu(prev_cpu, p->cpus_ptr)) {
      scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL_ON | prev_cpu, SCX_SLICE_DFL, 0);
      return prev_cpu;
    }
  } else if (concurrency_mode == 1) {
    const s32 this_cpu = (s32)bpf_get_smp_processor_id();
    const s32 node = __COMPAT_scx_bpf_cpu_node(this_cpu);

    cpu = __COMPAT_scx_bpf_pick_idle_cpu_node(p->cpus_ptr, node, 0);
    if (cpu < 0)
      cpu = __COMPAT_scx_bpf_pick_any_cpu_node(p->cpus_ptr, node, 0);

    if (cpu >= 0) {
      scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL_ON | cpu, SCX_SLICE_DFL, 0);
      return cpu;
    }
  }

  cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);
  if (is_idle)
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);

  return cpu;
}

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
  return scx_bpf_create_dsq(SHARED_DSQ, -1);
}

void BPF_STRUCT_OPS(lb_simple_exit, struct scx_exit_info *ei) {
  UEI_RECORD(uei, ei);
}

SCX_OPS_DEFINE(lb_simple_ops, .select_cpu = (void *)lb_simple_select_cpu,
               .init = (void *)lb_simple_init, .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
