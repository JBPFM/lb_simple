// SPDX-License-Identifier: GPL-2.0-only
//
// Copyright (c) 2024 Andrea Righi <andrea.righi@linux.dev>

mod bpf_skel;
pub use bpf_skel::*;
pub mod bpf_intf;

use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use libbpf_rs::Link;
use libbpf_rs::OpenObject;
use libc::pid_t;
use log::info;
use scx_utils::scx_ops_attach;
use scx_utils::scx_ops_load;
use scx_utils::scx_ops_open;

const SCHEDULER_NAME: &str = "lb_simple";

/// lb_simple: A simple global weighted vtime scheduler
#[derive(Debug, Parser, Clone)]
#[command(trailing_var_arg = true)]
struct Opts {
    /// Enable verbose output including periodic statistics
    #[clap(short = 'v', long, action = clap::ArgAction::SetTrue)]
    verbose: bool,

    /// Print scheduler statistics every INTERVAL seconds
    #[clap(short = 'i', long, default_value = "2")]
    interval: u64,

    /// Enable debug output
    #[clap(short = 'd', long, action = clap::ArgAction::SetTrue)]
    debug: bool,

    /// Optional command to launch (use -- to separate)
    #[clap(value_name = "CMD", last = true)]
    command: Vec<String>,
}

struct Scheduler {
    _link: Link,
}

impl Scheduler {
    fn init(opts: Opts, child_pid: Option<pid_t>, open_object: &mut MaybeUninit<OpenObject>) -> Result<Self> {
        // Initialize libbpf logging
        let mut skel_builder = BpfSkelBuilder::default();
        skel_builder.obj_builder.debug(opts.debug);

        // Open the BPF skeleton
        let mut skel = scx_ops_open!(skel_builder, open_object, lb_simple_ops, None)?;

        // Set BPF variables before loading
        if let Some(rodata) = &mut skel.maps.rodata_data {
            rodata.pid_filter = child_pid.unwrap_or(0);
        }

        // Load the BPF program
        let mut skel = scx_ops_load!(skel, lb_simple_ops, uei)?;

        // Attach the scheduler
        let _link = scx_ops_attach!(skel, lb_simple_ops)?;

        info!("{SCHEDULER_NAME} scheduler started");
        Ok(Self { _link })
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        info!("{SCHEDULER_NAME} scheduler stopped");
    }
}



#[derive(Debug, Clone, Copy)]
struct ChildProcess {
    pid: pid_t,
}

impl ChildProcess {
    fn spawn_suspended(command: &[String]) -> Result<Self> {
        if command.is_empty() {
            bail!("launch command is empty");
        }

        let cstrings: Vec<CString> = command
            .iter()
            .map(|arg| {
                CString::new(arg.as_str())
                    .with_context(|| format!("Argument '{}' contains interior NUL byte", arg))
            })
            .collect::<Result<_, _>>()?;

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error()).with_context(|| "fork failed");
        }

        if pid == 0 {
            unsafe {
                if libc::raise(libc::SIGSTOP) != 0 {
                    libc::_exit(127);
                }

                let mut argv: Vec<*const libc::c_char> =
                    cstrings.iter().map(|s| s.as_ptr()).collect();
                argv.push(ptr::null());

                libc::execvp(cstrings[0].as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }

        wait_for_child_stop(pid)?;

        Ok(Self { pid })
    }

    fn resume(&self) -> Result<()> {
        let ret = unsafe { libc::kill(self.pid, libc::SIGCONT) };
        if ret != 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("Failed to send SIGCONT to {}", self.pid));
        }
        Ok(())
    }

    fn wait(self) -> Result<()> {
        let mut status: libc::c_int = 0;
        loop {
            let ret = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if ret < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err)
                    .with_context(|| format!("waitpid failed for child {}", self.pid));
            }
            break;
        }

        if libc::WIFEXITED(status) {
            let exit_code = libc::WEXITSTATUS(status);
            info!("Child process {} exited with status {}", self.pid, exit_code);
        } else if libc::WIFSIGNALED(status) {
            let signal = libc::WTERMSIG(status);
            info!("Child process {} terminated by signal {}", self.pid, signal);
        }

        Ok(())
    }
}

fn wait_for_child_stop(pid: pid_t) -> Result<()> {
    let mut status: libc::c_int = 0;
    loop {
        let ret = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err)
                .with_context(|| format!("waitpid failed while waiting for {} to stop", pid));
        }

        if libc::WIFSTOPPED(status) {
            return Ok(());
        }

        if libc::WIFEXITED(status) {
            bail!(
                "child {} exited with status {} before SIGCONT",
                pid,
                libc::WEXITSTATUS(status)
            );
        }

        if libc::WIFSIGNALED(status) {
            bail!(
                "child {} terminated by signal {} before SIGCONT",
                pid,
                libc::WTERMSIG(status)
            );
        }
    }
}

fn main() -> Result<()> {
    // Parse command line arguments
    let opts = Opts::parse();

    // 参数约束：必须输入二进制文件和参数
    if opts.command.is_empty() {
        eprintln!("错误：必须指定要运行的二进制文件和参数");
        eprintln!("用法: {} [选项] -- <命令> [参数...]", std::env::args().next().unwrap_or_else(|| "lb_simple".to_string()));
        eprintln!("示例: {} -- /bin/ls -la", std::env::args().next().unwrap_or_else(|| "lb_simple".to_string()));
        std::process::exit(1);
    }

    // Initialize logger
    let log_level = if opts.debug {
        simplelog::LevelFilter::Debug
    } else {
        simplelog::LevelFilter::Info
    };

    simplelog::TermLogger::init(
        log_level,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Auto,
    )
    .context("Failed to initialize logger")?;

    // Setup signal handler for graceful shutdown
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::Relaxed);
    })
    .context("Failed to set Ctrl-C handler")?;

    let child = Some(ChildProcess::spawn_suspended(&opts.command)?);
    let child_pid = child.map(|c| c.pid);

    // Allocate open_object for the lifetime of the scheduler
    let mut open_object = MaybeUninit::uninit();

    // Initialize and run the scheduler
    let _sched = Scheduler::init(opts, child_pid, &mut open_object)?;

    // Resume and wait for child process
    let child = child.unwrap(); // Safe because we checked command is not empty
    child.resume()?;
    child.wait()?;
    info!("Child process completed, exiting scheduler");
    
    Ok(())
}
