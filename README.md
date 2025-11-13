# lb_simple

一个简单的全局加权虚拟时间调度器，基于 eBPF 和 sched_ext 框架实现。

## 功能特性

- 基于 eBPF 的进程调度器
- 支持为特定进程启用调度策略
- 使用虚拟时间（vtime）进行公平调度
- 支持详细的调试和统计信息输出

## 系统要求

- Linux 内核支持 sched_ext（通常需要 6.6+ 版本）
- Rust 工具链（edition 2024）
- libbpf 开发库
- root 权限（运行 eBPF 程序需要）

## 构建

```bash
cargo build --release
```

### 开发环境配置

项目提供了两个辅助脚本来配置 IDE 的代码补全和语法检查功能：

#### gen-compile-commands.sh

生成 `compile_commands.json` 文件，用于 IDE 理解 BPF C 代码的编译配置。

```bash
./gen-compile-commands.sh
```

该脚本会：
- 查找最新的构建输出目录
- 提取 BPF 头文件路径
- 生成包含正确编译标志的 `compile_commands.json`

#### update-clangd.sh

更新 `.clangd` 配置文件，为 clangd 语言服务器提供正确的 BPF 代码分析配置。

```bash
./update-clangd.sh
```

该脚本会：
- 自动检测最新的构建目录
- 配置 BPF 目标架构和头文件路径
- 设置适合 BPF 代码的编译器警告选项
- 禁用不适用于 BPF 的诊断信息

**注意：** 首次使用这些脚本前，需要先运行 `cargo build` 生成必要的Debug文件。

## 使用方法

**注意：程序必须指定要运行的二进制文件和参数，否则会直接退出。**

### 基本用法

```bash
sudo ./target/release/lb_simple -- <命令> [参数...]
```

### 命令行选项

- `-v, --verbose` - 启用详细输出，包括周期性统计信息
- `-i, --interval <秒>` - 每隔指定秒数打印调度器统计信息（默认：2秒）
- `-d, --debug` - 启用调试输出
- `--` - 分隔符，后面跟要运行的命令和参数

### 使用示例

1. 使用调度器运行 `ls` 命令：
```bash
sudo ./target/release/lb_simple -- /bin/ls -la
```

2. 启用详细输出运行程序：
```bash
sudo ./target/release/lb_simple -v -- /usr/bin/stress-ng --cpu 4 --timeout 10s
```

3. 启用调试模式并自定义统计间隔：
```bash
sudo ./target/release/lb_simple -d -i 5 -- /bin/bash -c "echo Hello World"
```

4. 运行 Python 脚本：
```bash
sudo ./target/release/lb_simple -- /usr/bin/python3 script.py
```


## 工作原理

1. 程序启动时会 fork 一个子进程并暂停（SIGSTOP）
2. 加载并附加 eBPF 调度器程序
3. 调度器开始监控指定的子进程
4. 恢复子进程执行（SIGCONT）
5. 等待子进程完成后退出

调度器使用 PID 过滤器只对指定的子进程及其派生进程应用调度策略。

## 许可证

GPL-2.0-only

## 作者

Copyright (c) 2024 Zhang Jiang

## 故障排除

### 权限错误
确保使用 `sudo` 运行程序，eBPF 程序需要 root 权限。

### 内核不支持 sched_ext
检查内核版本和配置：
```bash
uname -r
zgrep SCHED_CLASS_EXT /proc/config.gz
```

### 缺少参数错误
如果看到"错误：必须指定要运行的二进制文件和参数"，请确保使用 `--` 分隔符并提供要运行的命令。
