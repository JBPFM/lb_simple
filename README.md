# sched_ext BPF 调度器模板

这是一个用于快速创建 Linux sched_ext BPF 调度器项目的 `cargo-generate` 模板。

## 简介

此模板提供了一个完整的、可运行的 sched_ext 调度器作为模板，可以在此基础上编写自己的调度器，包含：

- **Rust 用户空间程序**：使用 `libbpf-rs` 和 `scx_utils` 管理 BPF 程序生命周期
- **eBPF 内核调度器**：实现了一个简单的调度器
- **统计信息收集**：跟踪本地和全局调度队列的任务分布
- **开发工具集成**：包含 clangd 配置生成脚本，支持 C 代码补全和跳转

## 快速开始

### 前置要求

确保系统满足以下条件：

1. **Linux 内核**：6.12+ 版本，且启用了 `CONFIG_SCHED_CLASS_EXT`
2. **Rust 工具链**：1.85+ 版本
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
3. **BPF 开发工具**：
   ```bash
   # Ubuntu/Debian
   sudo apt install clang llvm libelf-dev libbpf-dev

   # Fedora/RHEL
   sudo dnf install clang llvm elfutils-libelf-devel libbpf-devel

   # Arch Linux
   sudo pacman -S clang llvm libelf libbpf
   ```
4. **cargo-generate**：用于从模板创建项目
   ```bash
   cargo install cargo-generate
   ```

### 使用模板创建新项目

```bash
# 从 Git 仓库创建（如果已发布）
cargo generate --git https://github.com/JBPFM/scx_template.git

# 按提示输入配置：
# - project-name: 项目名称（如 scx_my_scheduler）
# - scheduler_slug: BPF 符号名称（如 my_scheduler，使用 snake_case）
# - license_id: SPDX 许可证标识符（默认 GPL-2.0-only）
```

### 构建和运行

```bash
# 进入生成的项目目录
cd your-project-name

# 构建项目（会自动编译 BPF 和 Rust 代码）
cargo build --release

# 以 root 权限运行调度器
sudo ./target/release/your-project-name
```

### 使用示例

```bash
sudo ./target/release/your-project-name
```

## 项目结构

```
scx_template/
├── src/
│   ├── main.rs              # Rust 用户空间程序入口
│   ├── bpf_skel.rs          # BPF skeleton（自动生成）
│   ├── bpf_intf.rs          # BPF 接口绑定（自动生成）
│   └── bpf/
│       ├── main.bpf.c       # eBPF 调度器实现
│       └── intf.h           # BPF 与用户空间共享的接口定义
├── build.rs                 # 构建脚本（编译 BPF 代码）
├── Cargo.toml              # Rust 依赖配置
├── cargo-generate.toml     # 模板配置
├── gen-compile-commands.sh # 生成 compile_commands.json
└── update-clangd.sh        # 更新 clangd 配置
```

### 核心文件说明

- **src/main.rs**：调度器用户空间主程序
  - 解析命令行参数
  - 加载并附加 BPF 程序
  - 收集和报告统计信息
  - 处理信号和优雅关闭

- **src/bpf/main.bpf.c**：BPF 内核调度器
  - `select_cpu`：为任务选择 CPU
  - `enqueue`：任务入队调度队列
  - `dispatch`：从队列分发任务到 CPU
  - `running`：任务开始执行时的回调
  - `stopping`：任务停止执行时的回调（更新 vtime）
  - `enable`：新任务加入时初始化
  - `init`：调度器初始化
  - `exit`：调度器退出清理

- **src/bpf/intf.h**：定义 BPF 和用户空间共享的数据结构

## 开发工具配置

### 为 C 代码配置 clangd

模板提供了脚本来生成 `compile_commands.json` 和更新 clangd 配置：

```bash
cargo build --debug -j6
# 生成 compile_commands.json（用于 C 代码补全）
./gen-compile-commands.sh

# 更新 .clangd 配置文件
./update-clangd.sh
```

这将启用：
- BPF 头文件路径补全
- 代码跳转（go-to-definition）
- 语法检查和警告

### 修改调度策略

1. **添加新的命令行选项**：
   - 编辑 `src/main.rs` 的 `Opts` 结构体
   - 添加 clap 属性定义新参数

2. **修改 BPF 调度逻辑**：
   - 编辑 `src/bpf/main.bpf.c`
   - 可以修改任务入队、选择、优先级等策略
   - 需要了解 sched_ext 的回调接口

3. **添加用户空间与 BPF 通信**：
   - 在 `src/bpf/intf.h` 中定义共享数据结构
   - 在 BPF 代码中创建 map（如 `BPF_MAP_TYPE_ARRAY`）
   - 在 Rust 代码中通过 `skel.maps.your_map` 访问

## 参考资源

- [sched_ext 官方文档](https://docs.kernel.org/scheduler/sched-ext.html)
- [scx_utils 库文档](https://docs.rs/scx_utils/)
- [libbpf-rs 文档](https://docs.rs/libbpf-rs/)
- [eBPF 开发指南](https://ebpf.io/)
- [Linux 调度器设计](https://www.kernel.org/doc/html/latest/scheduler/)

## 鸣谢
- 基于 Meta 的 sched_ext 框架
- 参考了 Andrea Righi 的调度器实现
- 使用了 Tejun Heo 和 David Vernet 的设计思想
