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

  /*
   * 只对指定进程生效，避免影响系统线程（例如 kworker），否则可能触发
   * sched_ext watchdog 的 runnable task stall。
   */
  if (pid_filter != 0) {
    const u32 tgid = (u32)BPF_CORE_READ(p, tgid);
    if (tgid != (u32)pid_filter)
      goto dfl;
  }

  if (concurrency_mode == 2) {
    if (prev_cpu >= 0 && bpf_cpumask_test_cpu(prev_cpu, p->cpus_ptr)) {
      scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL_ON | prev_cpu, SCX_SLICE_DFL, 0);
      return prev_cpu;
    }
  } else if (concurrency_mode == 1) {
    /*
     * 不使用 scx_bpf_pick_*_cpu_node()：在某些内核/配置下 per-node idle
     * tracking 被禁用时，调用会触发 runtime error 并导致调度器被禁用。
     */
    const s32 this_cpu = (s32)bpf_get_smp_processor_id();
    const s32 base_cpu =
        (prev_cpu >= 0 && bpf_cpumask_test_cpu(prev_cpu, p->cpus_ptr))
            ? prev_cpu
            : this_cpu;
    const s32 node = __COMPAT_scx_bpf_cpu_node(base_cpu);
    const struct cpumask *idle = scx_bpf_get_idle_cpumask();

    /* 先在 node 内找 idle CPU */
    for (s32 i = 0; i < NR_CPUS; i++) {
      if (!bpf_cpumask_test_cpu(i, p->cpus_ptr))
        continue;
      if (__COMPAT_scx_bpf_cpu_node(i) != node)
        continue;
      if (bpf_cpumask_test_cpu(i, idle)) {
        cpu = i;
        goto numa_found;
      }
    }

    /* node 内没有 idle CPU，则随便挑一个 */
    for (s32 i = 0; i < NR_CPUS; i++) {
      if (!bpf_cpumask_test_cpu(i, p->cpus_ptr))
        continue;
      if (__COMPAT_scx_bpf_cpu_node(i) != node)
        continue;
      cpu = i;
      goto numa_found;
    }

    scx_bpf_put_idle_cpumask(idle);
    goto dfl;

  numa_found:
    scx_bpf_put_idle_cpumask(idle);
    scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL_ON | cpu, SCX_SLICE_DFL, 0);
    return cpu;
  }

dfl:
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
