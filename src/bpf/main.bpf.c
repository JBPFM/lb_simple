/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * lb_simple - sched_ext lock handoff scheduler.
 *
 * Key points:
 * - Lock-contended yields are routed into lock-dedicated VIP DSQs.
 * - Unlock handoff records a per-cpu hint and dispatch moves from that DSQ.
 * - For sched_yield() (to == NULL), ops.yield() return value is ignored by
 *   the core and only used here for yield_to() compatibility.
 */
#include <scx/common.bpf.h>

#include "intf.h"

char _license[] SEC("license") = "GPL";

UEI_DEFINE(uei);

struct yield_addr_entry {
    __u64 user_ptr;
    __u32 last_gen;
    __u8 bpf_reason;
    __u8 pad[3];
    __u64 vip_dsq_id;
};

struct handoff_hint {
    __u64 vip_dsq_id;
    __u8 pending;
    __u8 pad[7];
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, __u32); /* pid (tid) */
    __type(value, struct yield_addr_entry);
} yield_addr_map SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, VIP_MAX_CPUS);
    __type(key, __u32);
    __type(value, struct handoff_hint);
} handoff_hint_map SEC(".maps");

static __always_inline __u32 vip_cpu_slot(__s32 cpu) {
    if (cpu < 0)
        return 0;
    return ((__u32)cpu) % VIP_MAX_CPUS;
}

static __always_inline bool vip_dsq_valid(__u64 dsq_id) {
    return dsq_id >= VIP_DSQ_BASE && dsq_id <= VIP_DSQ_LAST;
}

static __always_inline bool read_task_yield_info_seq(__u64 user_ptr,
                                                      struct task_yield_info *out) {
    __u32 gen1 = 0;
    __u32 gen2 = 0;
    __u64 gen_addr = user_ptr + sizeof(__u32);

    if (!user_ptr)
        return false;

    if (bpf_probe_read_user(&gen1, sizeof(gen1), (void *)(unsigned long)gen_addr))
        return false;
    if (gen1 & 1)
        return false;

    if (bpf_probe_read_user(out, sizeof(*out), (void *)(unsigned long)user_ptr))
        return false;

    if (bpf_probe_read_user(&gen2, sizeof(gen2), (void *)(unsigned long)gen_addr))
        return false;
    if ((gen2 & 1) || gen1 != gen2)
        return false;

    out->gen = gen2;
    return true;
}

s32 BPF_STRUCT_OPS(lb_simple_select_cpu, struct task_struct *p, s32 prev_cpu,
                   u64 wake_flags) {
    bool is_idle = false;
    s32 cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);

    if (is_idle)
        scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);

    return cpu;
}

bool BPF_STRUCT_OPS(lb_simple_yield, struct task_struct *from,
                    struct task_struct *to) {
    __u32 pid = from->pid;
    struct yield_addr_entry *entry;
    struct handoff_hint *hint;
    struct task_yield_info uinfo = {};
    __u32 cpu_slot;

    entry = bpf_map_lookup_elem(&yield_addr_map, &pid);
    if (!entry || !entry->user_ptr)
        return false;

    if (!read_task_yield_info_seq(entry->user_ptr, &uinfo) &&
        !read_task_yield_info_seq(entry->user_ptr, &uinfo))
        return false;

    if (uinfo.gen == entry->last_gen)
        return false;

    entry->last_gen = uinfo.gen;

    if (uinfo.reason == YIELD_LOCK_CONTENTION) {
        if (!vip_dsq_valid(uinfo.vip_dsq_id)) {
            entry->bpf_reason = YIELD_NONE;
            return false;
        }

        entry->vip_dsq_id = uinfo.vip_dsq_id;
        entry->bpf_reason = YIELD_LOCK_CONTENTION;
        return true;
    }

    if (uinfo.reason == YIELD_LOCK_HANDOFF) {
        if (!vip_dsq_valid(uinfo.vip_dsq_id)) {
            entry->bpf_reason = YIELD_NONE;
            return false;
        }

        cpu_slot = vip_cpu_slot(bpf_get_smp_processor_id());
        hint = bpf_map_lookup_elem(&handoff_hint_map, &cpu_slot);
        if (hint) {
            hint->vip_dsq_id = uinfo.vip_dsq_id;
            hint->pending = 1;
        }
        entry->bpf_reason = YIELD_NONE;

        /*
         * For sched_yield() (to == NULL), the core ignores this return value.
         * We only use return true to keep yield_to() semantics compatible.
         */
        return true;
    }

    entry->bpf_reason = YIELD_NONE;
    return false;
}

void BPF_STRUCT_OPS(lb_simple_enqueue, struct task_struct *p, u64 enq_flags) {
    __u32 pid = p->pid;
    struct yield_addr_entry *entry;

    entry = bpf_map_lookup_elem(&yield_addr_map, &pid);
    if (entry && entry->bpf_reason == YIELD_LOCK_CONTENTION &&
        vip_dsq_valid(entry->vip_dsq_id)) {
        scx_bpf_dsq_insert(p, entry->vip_dsq_id, SCX_SLICE_DFL, enq_flags);
        entry->bpf_reason = YIELD_NONE;
        return;
    }

    scx_bpf_dsq_insert(p, SCX_DSQ_GLOBAL, SCX_SLICE_DFL, enq_flags);
}

void BPF_STRUCT_OPS(lb_simple_dispatch, s32 cpu, struct task_struct *prev) {
    __u32 cpu_slot = vip_cpu_slot(cpu);
    struct handoff_hint *hint;

    hint = bpf_map_lookup_elem(&handoff_hint_map, &cpu_slot);
    if (hint && hint->pending) {
        if (!vip_dsq_valid(hint->vip_dsq_id)) {
            hint->pending = 0;
        } else if (scx_bpf_dsq_move_to_local(hint->vip_dsq_id)) {
            hint->pending = 0;
            return;
        }
    }
}

s32 BPF_STRUCT_OPS_SLEEPABLE(lb_simple_init) {
    __u32 slot;
    s32 ret;

    for (slot = 0; slot < VIP_DSQ_SLOTS; slot++) {
        ret = scx_bpf_create_dsq(VIP_DSQ_ID(slot), -1);
        if (ret)
            return ret;
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
               .yield = (void *)lb_simple_yield,
               .init = (void *)lb_simple_init,
               .exit = (void *)lb_simple_exit,
               .name = "lb_simple");
