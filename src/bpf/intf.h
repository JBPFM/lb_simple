/**
 * intf.h - BPF 程序与用户空间的接口定义
 *
 * 此文件定义了 BPF 调度器和 Rust 用户空间程序之间共享的数据结构。
 *
 * 使用说明：
 * 1. 在此文件中定义的结构体会自动生成 Rust 绑定（bpf_intf.rs）
 * 2. 可以在 BPF 程序（main.bpf.c）和 Rust 代码（main.rs）中使用
 * 3. 修改此文件后需要重新编译才能生效
 *
 * 示例用法：
 *   // 定义共享结构体
 *   struct task_stats {
 *       u64 runtime;
 *       u32 switches;
 *   };
 *
 *   // BPF 侧使用（main.bpf.c）
 *   struct task_stats stats = { .runtime = 0, .switches = 0 };
 *
 *   // Rust 侧使用（main.rs）
 *   use crate::bpf_intf::task_stats;
 *   let stats = task_stats { runtime: 0, switches: 0 };
 */

#ifndef __INTF_H
#define __INTF_H

#include <limits.h>

/*
 * 在此处添加你的自定义数据结构
 *
 * 例如：
 * struct scheduler_config {
 *     u64 time_slice_ns;
 *     bool enable_preemption;
 * };
 */

#endif /* __INTF_H */