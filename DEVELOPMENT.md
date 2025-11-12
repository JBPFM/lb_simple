# 开发指南

本文档为使用此模板开发自定义 sched_ext 调度器的开发者提供详细指导。

## 目录

- [开发环境设置](#开发环境设置)
- [项目架构深入](#项目架构深入)
- [BPF 程序开发](#bpf-程序开发)
- [用户空间程序开发](#用户空间程序开发)
- [调试技术](#调试技术)
- [性能分析](#性能分析)
- [测试策略](#测试策略)
- [最佳实践](#最佳实践)

## 开发环境设置

### IDE 配置

#### VSCode 推荐设置

创建 `.vscode/settings.json`：

```json
{
  "rust-analyzer.cargo.features": "all",
  "rust-analyzer.checkOnSave.command": "clippy",
  "C_Cpp.default.configurationProvider": "llvm-vs-code-extensions.vscode-clangd",
  "clangd.arguments": [
    "--compile-commands-dir=${workspaceFolder}",
    "--header-insertion=never"
  ]
}
```

创建 `.vscode/tasks.json`：

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "cargo build",
      "type": "shell",
      "command": "cargo build",
      "problemMatcher": ["$rustc"],
      "group": {
        "kind": "build",
        "isDefault": true
      }
    },
    {
      "label": "Update clangd",
      "type": "shell",
      "command": "./gen-compile-commands.sh && ./update-clangd.sh"
    }
  ]
}
```

#### Neovim/Vim 配置

使用 LSP 配置（需要 `nvim-lspconfig`）：

```lua
-- Rust
require('lspconfig').rust_analyzer.setup{
  settings = {
    ['rust-analyzer'] = {
      cargo = {
        features = "all"
      },
      checkOnSave = {
        command = "clippy"
      }
    }
  }
}

-- C/BPF
require('lspconfig').clangd.setup{
  cmd = {
    "clangd",
    "--compile-commands-dir=" .. vim.fn.getcwd(),
    "--header-insertion=never"
  }
}
```

### 构建系统理解

#### build.rs 工作流程

`build.rs` 在编译 Rust 代码前执行，负责：

1. **编译 BPF C 代码**：
   ```rust
   scx_cargo::BpfBuilder::new()?
       .enable_skel("src/bpf/main.bpf.c", "bpf")  // 生成 bpf_skel.rs
       .enable_intf("src/bpf/intf.h", "bpf_intf.rs")  // 生成 bpf_intf.rs
       .build()?;
   ```

2. **生成 Rust 绑定**：
   - `bpf_skel.rs`：BPF skeleton，用于加载和管理 BPF 程序
   - `bpf_intf.rs`：从 `intf.h` 生成的 Rust 类型定义

3. **触发重新编译条件**：
   - `src/bpf/*.c` 或 `*.h` 文件变更
   - `build.rs` 自身变更

#### 自定义构建配置

如果需要自定义 BPF 编译选项，修改 `build.rs`：

```rust
scx_cargo::BpfBuilder::new()
    .unwrap()
    .enable_intf("src/bpf/intf.h", "bpf_intf.rs")
    .enable_skel("src/bpf/main.bpf.c", "bpf")
    // 添加自定义 clang 标志
    .clang_args(&["-DMY_CUSTOM_DEFINE=1"])
    // 添加额外的头文件搜索路径
    .include_path("/path/to/extra/headers")
    .build()
    .unwrap();
```

## 项目架构深入

### 数据流图

```
┌─────────────────┐
│  用户空间 Rust  │
│   (main.rs)     │
└────────┬────────┘
         │ libbpf-rs
         │ (加载/通信)
         ↓
┌─────────────────┐
│  BPF 程序       │
│ (main.bpf.c)    │
└────────┬────────┘
         │ sched_ext ops
         │ (调度决策)
         ↓
┌─────────────────┐
│  内核调度器     │
│  (sched_ext)    │
└─────────────────┘
```

### 模块职责

| 文件 | 职责 | 编辑频率 |
|------|------|----------|
| `src/main.rs` | 程序入口、参数解析、BPF 生命周期管理、统计收集 | 中等 |
| `src/bpf/main.bpf.c` | 核心调度逻辑、任务选择、队列管理 | 高 |
| `src/bpf/intf.h` | BPF ↔ 用户空间共享数据结构 | 低 |
| `build.rs` | 构建配置、BPF 编译 | 低 |
| `src/bpf_skel.rs` | **自动生成，不要手动编辑** | 无 |
| `src/bpf_intf.rs` | **自动生成，不要手动编辑** | 无 |

## BPF 程序开发

### sched_ext 回调接口

完整的回调生命周期：

```c
// 1. 调度器初始化（只执行一次）
s32 BPF_STRUCT_OPS_SLEEPABLE(my_sched_init)
{
    // 创建 DSQ、初始化全局变量
    return scx_bpf_create_dsq(SHARED_DSQ, -1);
}

// 2. 任务加入系统（每个任务一次）
void BPF_STRUCT_OPS(my_sched_enable, struct task_struct *p)
{
    // 初始化任务特定的调度状态
    p->scx.dsq_vtime = vtime_now;
}

// 3. 任务被唤醒，选择运行的 CPU
s32 BPF_STRUCT_OPS(my_sched_select_cpu, struct task_struct *p,
                   s32 prev_cpu, u64 wake_flags)
{
    // 返回建议的 CPU，或让内核默认选择
    s32 cpu = scx_bpf_select_cpu_dfl(p, prev_cpu, wake_flags, &is_idle);

    // 如果 CPU 空闲，可以直接调度到本地队列
    if (is_idle) {
        scx_bpf_dsq_insert(p, SCX_DSQ_LOCAL, SCX_SLICE_DFL, 0);
    }

    return cpu;
}

// 4. 任务入队（每次任务就绪时）
void BPF_STRUCT_OPS(my_sched_enqueue, struct task_struct *p, u64 enq_flags)
{
    // 将任务插入调度队列
    scx_bpf_dsq_insert_vtime(p, SHARED_DSQ, SCX_SLICE_DFL,
                             p->scx.dsq_vtime, enq_flags);
}

// 5. CPU 需要新任务时调用
void BPF_STRUCT_OPS(my_sched_dispatch, s32 cpu, struct task_struct *prev)
{
    // 从全局队列移动任务到本地 CPU
    scx_bpf_dsq_move_to_local(SHARED_DSQ);
}

// 6. 任务开始执行
void BPF_STRUCT_OPS(my_sched_running, struct task_struct *p)
{
    // 更新运行时统计、vtime 等
}

// 7. 任务停止执行（时间片用完或阻塞）
void BPF_STRUCT_OPS(my_sched_stopping, struct task_struct *p, bool runnable)
{
    // 更新 vtime，计算执行时间
    p->scx.dsq_vtime += (SCX_SLICE_DFL - p->scx.slice) * 100 / p->scx.weight;
}

// 8. 任务离开系统（每个任务一次）
void BPF_STRUCT_OPS(my_sched_disable, struct task_struct *p)
{
    // 清理任务特定状态
}

// 9. 调度器退出（只执行一次）
void BPF_STRUCT_OPS(my_sched_exit, struct scx_exit_info *ei)
{
    // 记录退出信息
    UEI_RECORD(uei, ei);
}
```

### 常用 BPF 辅助函数

#### 队列操作

```c
// 插入任务到队列（FIFO）
scx_bpf_dsq_insert(struct task_struct *p, u64 dsq_id,
                   u64 slice, u64 enq_flags);

// 插入任务到队列（按 vtime 排序）
scx_bpf_dsq_insert_vtime(struct task_struct *p, u64 dsq_id,
                         u64 slice, u64 vtime, u64 enq_flags);

// 移动任务到本地 CPU
scx_bpf_dsq_move_to_local(u64 dsq_id);

// 创建自定义 DSQ
scx_bpf_create_dsq(u64 dsq_id, s32 node);
```

#### 任务信息获取

```c
// 获取任务权重
p->scx.weight

// 获取任务 vtime
p->scx.dsq_vtime

// 获取剩余时间片
p->scx.slice

// 获取任务 PID
p->pid

// 获取任务优先级
p->prio
```

#### CPU 拓扑

```c
// 默认 CPU 选择（考虑缓存、NUMA）
s32 scx_bpf_select_cpu_dfl(struct task_struct *p, s32 prev_cpu,
                           u64 wake_flags, bool *is_idle);

// 测试 CPU 是否在 cpumask 中
bool bpf_cpumask_test_cpu(u32 cpu, const struct cpumask *cpumask);
```

### 添加自定义 Map

#### 在 BPF 侧定义

```c
// src/bpf/main.bpf.c

// 示例：跟踪每个任务的执行时间
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(key_size, sizeof(u32));    // PID
    __uint(value_size, sizeof(u64));  // 执行时间
    __uint(max_entries, 10240);
} task_runtime SEC(".maps");

void BPF_STRUCT_OPS(my_sched_running, struct task_struct *p)
{
    u32 pid = p->pid;
    u64 *runtime = bpf_map_lookup_elem(&task_runtime, &pid);
    if (runtime) {
        // 更新运行时
        *runtime += SCX_SLICE_DFL;
    } else {
        // 初始化
        u64 init_val = SCX_SLICE_DFL;
        bpf_map_update_elem(&task_runtime, &pid, &init_val, BPF_ANY);
    }
}
```

#### 在 Rust 侧访问

```rust
// src/main.rs

impl<'a> Scheduler<'a> {
    fn read_task_runtime(&self, pid: u32) -> Result<u64> {
        let map = &self.skel.maps.task_runtime;
        let key = pid.to_ne_bytes();

        if let Some(value) = map.lookup(&key, libbpf_rs::MapFlags::ANY)? {
            Ok(u64::from_ne_bytes(value.try_into()?))
        } else {
            Ok(0)
        }
    }
}
```

### 定义共享数据结构

在 `src/bpf/intf.h` 中定义：

```c
// src/bpf/intf.h
#ifndef __INTF_H
#define __INTF_H

// 自定义任务属性
struct task_ctx {
    u64 vtime;
    u64 runtime;
    u32 priority;
};

// 调度器配置
struct sched_config {
    bool enable_feature_x;
    u32 time_slice_ns;
};

#endif /* __INTF_H */
```

在 BPF 和 Rust 中使用：

```c
// BPF 侧 (main.bpf.c)
#include "intf.h"

struct task_ctx ctx = {
    .vtime = 0,
    .runtime = 0,
    .priority = 5,
};
```

```rust
// Rust 侧 (main.rs)
use crate::bpf_intf::task_ctx;

let ctx = task_ctx {
    vtime: 0,
    runtime: 0,
    priority: 5,
};
```

## 用户空间程序开发

### 添加命令行选项

```rust
// src/main.rs

#[derive(Debug, Parser)]
struct Opts {
    /// 现有选项...

    /// 自定义时间片（微秒）
    #[clap(short = 's', long, default_value = "20000")]
    slice_us: u64,

    /// 启用高级调度特性
    #[clap(long, action = clap::ArgAction::SetTrue)]
    enable_advanced: bool,
}
```

### 将参数传递给 BPF

方法 1：通过 rodata（只读，初始化时设置）

```rust
// src/main.rs
impl<'a> Scheduler<'a> {
    fn init(opts: Opts, open_object: &'a mut MaybeUninit<OpenObject>) -> Result<Self> {
        // ...
        if let Some(rodata) = &mut skel.maps.rodata_data {
            rodata.fifo_sched = opts.fifo;
            rodata.slice_us = opts.slice_us;  // 添加新参数
        }
        // ...
    }
}
```

```c
// src/bpf/main.bpf.c
const volatile bool fifo_sched;
const volatile u64 slice_us;  // 对应 Rust 设置的值
```

方法 2：通过 Map（运行时可修改）

```rust
// 更新配置 map
fn update_config(&mut self, new_slice: u64) -> Result<()> {
    let map = &self.skel.maps.config;
    let key = 0u32.to_ne_bytes();
    let value = new_slice.to_ne_bytes();
    map.update(&key, &value, libbpf_rs::MapFlags::ANY)?;
    Ok(())
}
```

### 实现周期性统计

```rust
use std::time::{Duration, Instant};

impl<'a> Scheduler<'a> {
    fn run(&mut self, shutdown: Arc<AtomicBool>) -> Result<()> {
        let mut last_print = Instant::now();
        let interval = Duration::from_secs(self.opts.interval);

        while !shutdown.load(Ordering::Relaxed) && !uei_exited!(&self.skel, uei) {
            std::thread::sleep(Duration::from_millis(100));

            if last_print.elapsed() >= interval {
                if self.opts.verbose {
                    self.print_stats()?;
                }
                last_print = Instant::now();
            }
        }

        uei_report!(&self.skel, uei)?;
        Ok(())
    }
}
```

## 调试技术

### BPF 程序调试

#### 1. 使用 bpf_printk

```c
// src/bpf/main.bpf.c

void BPF_STRUCT_OPS(my_sched_enqueue, struct task_struct *p, u64 enq_flags)
{
    // 添加调试输出
    bpf_printk("Enqueue task PID=%d vtime=%llu", p->pid, p->scx.dsq_vtime);

    scx_bpf_dsq_insert_vtime(p, SHARED_DSQ, SCX_SLICE_DFL,
                             p->scx.dsq_vtime, enq_flags);
}
```

查看输出：
```bash
sudo cat /sys/kernel/debug/tracing/trace_pipe | grep my_sched
```

#### 2. 使用 bpftool 检查

```bash
# 列出加载的 BPF 程序
sudo bpftool prog list | grep sched

# 查看程序详情
sudo bpftool prog show id <prog_id>

# Dump BPF 程序字节码
sudo bpftool prog dump xlated id <prog_id>

# 列出所有 map
sudo bpftool map list

# Dump map 内容
sudo bpftool map dump id <map_id>

# 持续监控 map 变化
watch -n 1 'sudo bpftool map dump id <map_id>'
```

#### 3. 验证器日志

如果 BPF 程序加载失败，启用详细日志：

```rust
// src/main.rs
let mut skel_builder = BpfSkelBuilder::default();
skel_builder.obj_builder.debug(true);  // 启用调试
```

或使用环境变量：
```bash
RUST_LOG=debug sudo ./your-scheduler
```

### Rust 程序调试

#### GDB 调试

```bash
# 编译带调试符号的版本
cargo build

# 使用 GDB
sudo gdb --args ./target/debug/your-scheduler -v

# 常用 GDB 命令
(gdb) break main                    # 在 main 设置断点
(gdb) break Scheduler::run          # 在方法设置断点
(gdb) run                           # 运行
(gdb) continue                      # 继续执行
(gdb) print variable_name           # 打印变量
(gdb) backtrace                     # 查看调用栈
```

#### LLDB 调试（推荐用于 Rust）

```bash
sudo lldb ./target/debug/your-scheduler -- -v

(lldb) breakpoint set --name main
(lldb) breakpoint set --method run
(lldb) run
(lldb) print variable_name
(lldb) thread backtrace
```

### 日志级别控制

```rust
// src/main.rs

// 根据需要调整日志级别
let log_level = if opts.debug {
    simplelog::LevelFilter::Debug
} else if opts.verbose {
    simplelog::LevelFilter::Info
} else {
    simplelog::LevelFilter::Warn
};

// 在代码中使用不同级别
log::trace!("非常详细的调试信息");
log::debug!("调试信息");
log::info!("一般信息");
log::warn!("警告");
log::error!("错误");
```

## 性能分析

### BPF 性能分析

#### 1. 测量 BPF 函数执行时间

```c
// src/bpf/main.bpf.c

void BPF_STRUCT_OPS(my_sched_enqueue, struct task_struct *p, u64 enq_flags)
{
    u64 start = bpf_ktime_get_ns();

    // 你的调度逻辑
    scx_bpf_dsq_insert_vtime(p, SHARED_DSQ, SCX_SLICE_DFL,
                             p->scx.dsq_vtime, enq_flags);

    u64 duration = bpf_ktime_get_ns() - start;

    // 记录到 map 或打印
    bpf_printk("enqueue took %llu ns", duration);
}
```

#### 2. 使用 perf 分析

```bash
# 记录调度器性能数据
sudo perf record -e sched:* -a -g -- sleep 10

# 分析报告
sudo perf report

# 查看调度延迟
sudo perf sched latency

# 查看调度映射
sudo perf sched map
```

### 系统性能指标

监控调度器对系统的影响：

```bash
# 上下文切换率
vmstat 1

# CPU 使用率
mpstat -P ALL 1

# 进程调度延迟
perf sched latency

# 系统吞吐量测试
sysbench cpu --threads=4 --time=30 run
```

## 测试策略

### 单元测试

对于纯 Rust 逻辑（不涉及 BPF）：

```rust
// src/main.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opts_parsing() {
        let opts = Opts::parse_from(&["prog", "-f", "-v", "-i", "5"]);
        assert_eq!(opts.fifo, true);
        assert_eq!(opts.verbose, true);
        assert_eq!(opts.interval, 5);
    }
}
```

运行测试：
```bash
cargo test
```

### 集成测试

创建 `tests/integration_test.sh`：

```bash
#!/bin/bash
set -e

# 构建调度器
cargo build --release

# 启动调度器（后台）
sudo ./target/release/your-scheduler -v -i 1 &
SCHED_PID=$!

# 等待启动
sleep 2

# 运行工作负载
echo "Running workload..."
stress-ng --cpu 4 --timeout 10s

# 检查调度器是否仍在运行
if ps -p $SCHED_PID > /dev/null; then
    echo "✓ Scheduler survived workload"
else
    echo "✗ Scheduler crashed"
    exit 1
fi

# 优雅关闭
sudo kill -INT $SCHED_PID
wait $SCHED_PID

echo "✓ All tests passed"
```

### 压力测试

使用 `stress-ng` 进行压力测试：

```bash
# CPU 密集型
sudo stress-ng --cpu $(nproc) --timeout 60s

# I/O 密集型
sudo stress-ng --io 4 --timeout 60s

# 混合负载
sudo stress-ng --cpu 4 --io 2 --vm 2 --timeout 60s
```

### 对比测试

比较你的调度器与默认调度器：

```bash
#!/bin/bash

# 测试默认调度器
echo "Testing CFS..."
sysbench cpu --threads=8 --time=30 run > cfs_result.txt

# 测试你的调度器
echo "Testing custom scheduler..."
sudo ./your-scheduler &
SCHED_PID=$!
sleep 2
sysbench cpu --threads=8 --time=30 run > custom_result.txt
sudo kill -INT $SCHED_PID

# 比较结果
echo "CFS result:"
grep "total time:" cfs_result.txt
echo "Custom scheduler result:"
grep "total time:" custom_result.txt
```

## 最佳实践

### 代码组织

1. **保持 BPF 代码简单**：
   - 避免复杂的循环（验证器限制）
   - 使用内联函数减少调用开销
   - 注意栈空间限制（512 字节）

2. **错误处理**：
   ```rust
   // Rust 侧
   fn read_stats(&mut self) -> Result<Stats> {
       let stats = self.skel.maps.stats
           .lookup(&key, libbpf_rs::MapFlags::ANY)?
           .ok_or_else(|| anyhow!("Stats not found"))?;

       Ok(Stats::from_bytes(&stats)?)
   }
   ```

3. **资源清理**：
   ```rust
   impl<'a> Drop for Scheduler<'a> {
       fn drop(&mut self) {
           info!("Cleaning up scheduler resources");
           // BPF 程序会自动卸载（link 的 Drop）
       }
   }
   ```

### 性能优化

1. **减少 Map 查找**：
   ```c
   // 差：每次都查找
   void process_task(struct task_struct *p) {
       u64 *val = bpf_map_lookup_elem(&map, &key);
       if (val) (*val)++;
   }

   // 好：缓存查找结果
   void process_tasks(struct task_struct **tasks, int n) {
       u64 *val = bpf_map_lookup_elem(&map, &key);
       if (!val) return;

       for (int i = 0; i < n && i < 8; i++) {  // BPF 循环限制
           *val += process(tasks[i]);
       }
   }
   ```

2. **使用 per-CPU Map**：
   ```c
   // 避免跨 CPU 竞争
   struct {
       __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
       __uint(key_size, sizeof(u32));
       __uint(value_size, sizeof(u64));
       __uint(max_entries, 1);
   } percpu_stats SEC(".maps");
   ```

3. **批量操作**：
   ```rust
   // 批量读取 per-CPU 统计
   fn read_all_cpu_stats(&self) -> Result<Vec<u64>> {
       let stats = &self.skel.maps.percpu_stats;
       let key = 0u32.to_ne_bytes();

       if let Some(values) = stats.lookup_percpu(&key, libbpf_rs::MapFlags::ANY)? {
           values.iter()
               .map(|v| Ok(u64::from_ne_bytes(v.try_into()?)))
               .collect()
       } else {
           Ok(vec![])
       }
   }
   ```

### 安全考虑

1. **验证用户输入**：
   ```rust
   let opts = Opts::parse();

   if opts.slice_us == 0 || opts.slice_us > 1_000_000 {
       bail!("Invalid slice_us: must be between 1 and 1000000");
   }
   ```

2. **处理信号**：
   ```rust
   // 已经在模板中实现
   ctrlc::set_handler(move || {
       shutdown_clone.store(true, Ordering::Relaxed);
   })?;
   ```

3. **检查权限**：
   ```rust
   use std::os::unix::fs::PermissionsExt;

   if !nix::unistd::Uid::effective().is_root() {
       bail!("This program must be run as root");
   }
   ```

### 文档编写

在代码中添加充分的注释：

```c
// src/bpf/main.bpf.c

/**
 * my_sched_enqueue - 将任务加入调度队列
 * @p: 要调度的任务
 * @enq_flags: 入队标志（SCX_ENQ_* 常量）
 *
 * 根据调度模式（FIFO 或 vtime）将任务插入共享调度队列。
 * 对于 vtime 模式，限制空闲任务的预算积累。
 */
void BPF_STRUCT_OPS(my_sched_enqueue, struct task_struct *p, u64 enq_flags)
{
    // ...
}
```

```rust
// src/main.rs

/// 调度器统计信息
struct Stats {
    /// 本地队列调度次数
    local: u64,
    /// 全局队列调度次数
    global: u64,
}

impl Stats {
    /// 计算总调度次数
    fn total(&self) -> u64 {
        self.local + self.global
    }
}
```

## 常见开发任务

### 添加新的调度策略

1. 在 BPF 侧实现逻辑
2. 添加配置选项到 `Opts`
3. 通过 rodata 传递参数
4. 更新文档

示例：添加优先级调度

```c
// src/bpf/main.bpf.c

const volatile bool priority_sched;  // 新参数

void BPF_STRUCT_OPS(my_sched_enqueue, struct task_struct *p, u64 enq_flags)
{
    if (priority_sched) {
        // 使用 nice 值作为优先级
        u64 priority = 20 - p->prio;  // 转换为正值
        scx_bpf_dsq_insert_vtime(p, SHARED_DSQ, SCX_SLICE_DFL,
                                 priority, enq_flags);
    } else {
        // 原有逻辑
        // ...
    }
}
```

```rust
// src/main.rs

#[derive(Debug, Parser)]
struct Opts {
    // ...

    /// 启用基于优先级的调度
    #[clap(short = 'p', long, action = clap::ArgAction::SetTrue)]
    priority: bool,
}

// 在 init 中设置
if let Some(rodata) = &mut skel.maps.rodata_data {
    rodata.priority_sched = opts.priority;
}
```

### 添加实时统计导出

使用 `scx_stats` 导出指标：

```rust
use scx_stats::{ScxStatsData, ScxStatsServer};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, ScxStatsData)]
struct MyStats {
    local_dispatches: u64,
    global_dispatches: u64,
    avg_latency_us: f64,
}

// 在 Scheduler 中启动统计服务器
fn init_stats_server(&self) -> Result<ScxStatsServer<MyStats>> {
    ScxStatsServer::new()
        .add_stats_target("scheduler", Box::new(self.clone()))
        .serve("127.0.0.1:9000")
}
```

客户端可通过 HTTP API 获取：
```bash
curl http://localhost:9000/stats/scheduler
```

## 资源和进一步学习

- **sched_ext 文档**：[kernel.org/doc/html/latest/scheduler/sched-ext.html](https://www.kernel.org/doc/html/latest/scheduler/sched-ext.html)
- **BPF 文档**：[ebpf.io/documentation](https://ebpf.io/documentation)
- **libbpf-rs**：[github.com/libbpf/libbpf-rs](https://github.com/libbpf/libbpf-rs)
- **scx_utils 示例**：查看 Linux 内核源码中的 `tools/sched_ext/` 目录
- **BPF 验证器**：了解限制和优化技巧

## 获取帮助

如果遇到问题：

1. 检查 [README.md](README.md) 的故障排除部分
2. 查看内核日志：`sudo dmesg | tail`
3. 启用详细日志：`-d -v` 选项
4. 在项目 issue 中搜索类似问题
5. 提交 issue，包含：
   - 内核版本 (`uname -r`)
   - 完整错误信息
   - 复现步骤
   - 相关配置
