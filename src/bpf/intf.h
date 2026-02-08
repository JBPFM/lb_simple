/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef __INTF_H
#define __INTF_H

#ifndef __kptr
#define __kptr
#endif

#define VIP_DSQ_BASE 1ULL
#define VIP_DSQ_SLOTS 4096U
#define VIP_DSQ_LAST (VIP_DSQ_BASE + VIP_DSQ_SLOTS - 1ULL)
#define VIP_MAX_CPUS 256U
#define VIP_DSQ_ID(slot) (VIP_DSQ_BASE + (unsigned long long)(slot))

enum yield_reason {
    YIELD_NONE            = 0,
    YIELD_LOCK_CONTENTION = 1,
    YIELD_LOCK_HANDOFF    = 2,
};

/* Stored in user thread-local memory and sampled from BPF. */
struct task_yield_info {
    unsigned int reason; /* enum yield_reason */
    unsigned int gen;    /* seqcount: odd=writer active, even=committed */
    unsigned long long vip_dsq_id;
};

struct held_lock_info {
    unsigned long long lock_addr;
    unsigned long long hold_start_ns;
    unsigned int depth;
    unsigned int pad;
};

struct slice_track_info {
    unsigned long long slice_budget_ns;
    unsigned long long slice_start_ns;
    unsigned char near_exhaust;
    unsigned char pad[7];
};

enum stat_idx {
    STAT_FUTEX_WAIT = 0,
    STAT_FUTEX_WAKE = 1,
    STAT_HOLD_SWITCHOUT_TOTAL = 2,
    STAT_HOLD_SWITCHOUT_SLICE = 3,
    STAT_HOLD_SWITCHOUT_OTHER = 4,
    STAT_BOOST_APPLIED = 5,
    STAT_NR = 6,
};

#endif
