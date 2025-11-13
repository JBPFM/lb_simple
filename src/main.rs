// SPDX-License-Identifier: GPL-2.0-only
//
// Copyright (c) 2024 Andrea Righi <andrea.righi@linux.dev>

mod bpf_skel;
pub use bpf_skel::*;
pub mod bpf_intf;

use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use libbpf_rs::Link;
use libbpf_rs::MapCore;
use libbpf_rs::OpenObject;
use libc::pid_t;
use log::info;
use log::warn;
use scx_utils::scx_ops_attach;
use scx_utils::scx_ops_load;
use scx_utils::scx_ops_open;
use scx_utils::uei_exited;
use scx_utils::uei_report;

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

    /// Restrict futex tracking to the tasks inside this cgroup v2 path
    #[clap(long = "cgroup", value_name = "PATH")]
    cgroup: Option<PathBuf>,

    /// Optional command to launch inside the target cgroup (use -- to separate)
    #[clap(value_name = "CMD", last = true)]
    command: Vec<String>,
}

struct Scheduler<'a> {
    skel: BpfSkel<'a>,
    opts: Opts,
    _link: Link,
}

impl<'a> Scheduler<'a> {
    fn init(opts: Opts, open_object: &'a mut MaybeUninit<OpenObject>) -> Result<Self> {
        // Initialize libbpf logging
        let mut skel_builder = BpfSkelBuilder::default();
        skel_builder.obj_builder.debug(opts.debug);

        // Open the BPF skeleton
        let mut skel = scx_ops_open!(skel_builder, open_object, lb_simple_ops, None)?;

        // Set BPF variables before loading
        if let Some(rodata) = &mut skel.maps.rodata_data {
            rodata.use_cgroup_filter = opts.cgroup.is_some();
        }

        // Load the BPF program
        let mut skel = scx_ops_load!(skel, lb_simple_ops, uei)?;

        // Attach the scheduler
        let _link = scx_ops_attach!(skel, lb_simple_ops)?;

        info!("{SCHEDULER_NAME} scheduler started");
        Ok(Self { skel, opts, _link })
    }

    // fn read_stats(&mut self) -> Result<(u64, u64)> {
    //     let mut wait_total = 0u64;
    //     let mut wake_total = 0u64;
    //
    //     // Read per-CPU statistics
    //     let stats = &self.skel.maps.stats;
    //     let zero_key = 0u32.to_ne_bytes();
    //     let one_key = 1u32.to_ne_bytes();
    //
    //     // Sum up stats from all CPUs
    //     if let Ok(Some(vec_values)) = stats.lookup_percpu(&zero_key, libbpf_rs::MapFlags::ANY) {
    //         for value in vec_values.iter() {
    //             if value.len() >= 8 {
    //                 wait_total += u64::from_ne_bytes(value[0..8].try_into()?);
    //             }
    //         }
    //     }
    //
    //     if let Ok(Some(vec_values)) = stats.lookup_percpu(&one_key, libbpf_rs::MapFlags::ANY) {
    //         for value in vec_values.iter() {
    //             if value.len() >= 8 {
    //                 wake_total += u64::from_ne_bytes(value[0..8].try_into()?);
    //             }
    //         }
    //     }
    //
    //     Ok((wait_total, wake_total))
    // }

    // fn print_stats(&mut self) -> Result<()> {
    //     let (wait, wake) = self.read_stats()?;
    //     let total = wait + wake;
    //
    //     if total > 0 {
    //         info!(
    //             "Stats: wait={} ({:.1}%) wake={} ({:.1}%) total={}",
    //             wait,
    //             (wait as f64 / total as f64) * 100.0,
    //             wake,
    //             (wake as f64 / total as f64) * 100.0,
    //             total
    //         );
    //     }
    //
    //     Ok(())
    // }
    //
    // fn run(&mut self, shutdown: Arc<AtomicBool>) -> Result<()> {
    //     let interval = Duration::from_secs(self.opts.interval);
    //
    //     while !shutdown.load(Ordering::Relaxed) && !uei_exited!(&self.skel, uei) {
    //         std::thread::sleep(interval);
    //
    //         if self.opts.verbose {
    //             if let Err(e) = self.print_stats() {
    //                 warn!("Failed to print stats: {}", e);
    //             }
    //         }
    //     }
    //
    //     uei_report!(&self.skel, uei)?;
    //     Ok(())
    // }

    fn install_cgroup_filter(&mut self, cgroup_fd: RawFd) -> Result<()> {
        let key = 0u32.to_ne_bytes();
        let value = (cgroup_fd as i32).to_ne_bytes();
        self.skel
            .maps
            .cgroup_filter
            .update(&key, &value, libbpf_rs::MapFlags::ANY)?;
        Ok(())
    }
}

impl<'a> Drop for Scheduler<'a> {
    fn drop(&mut self) {
        info!("{SCHEDULER_NAME} scheduler stopped");
    }
}

struct CgroupContext {
    path: PathBuf,
    dir_fd: OwnedFd,
}

impl CgroupContext {
    fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        if !path_ref.exists() {
            bail!("cgroup path {} does not exist", path_ref.display());
        }

        let metadata = fs::metadata(path_ref)
            .with_context(|| format!("Failed to read metadata for {}", path_ref.display()))?;
        if !metadata.is_dir() {
            bail!("{} is not a directory", path_ref.display());
        }

        let dir = File::open(path_ref)
            .with_context(|| format!("Failed to open {}", path_ref.display()))?;

        Ok(Self {
            path: path_ref.to_path_buf(),
            dir_fd: OwnedFd::from(dir),
        })
    }

    fn assign_pid(&self, pid: pid_t) -> Result<()> {
        let mut procs = self.path.clone();
        procs.push("cgroup.procs");
        fs::write(&procs, format!("{pid}\n"))
            .with_context(|| format!("Failed to write pid {} into {}", pid, procs.display()))?;
        Ok(())
    }

    fn install_filter(&self, scheduler: &mut Scheduler) -> Result<()> {
        scheduler.install_cgroup_filter(self.dir_fd.as_raw_fd())
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

    fn reap_async(self) {
        std::thread::spawn(move || {
            let mut status: libc::c_int = 0;
            loop {
                let ret = unsafe { libc::waitpid(self.pid, &mut status, 0) };
                if ret < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                }
                break;
            }
        });
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

    if !opts.command.is_empty() && opts.cgroup.is_none() {
        bail!("Launching a command requires --cgroup PATH");
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

    let cgroup_ctx = if let Some(path) = opts.cgroup.as_ref() {
        Some(CgroupContext::new(path)?)
    } else {
        None
    };

    let child = if !opts.command.is_empty() {
        let child = ChildProcess::spawn_suspended(&opts.command)?;
        if let Some(ctx) = &cgroup_ctx {
            ctx.assign_pid(child.pid)?;
        }
        Some(child)
    } else {
        None
    };

    // Allocate open_object for the lifetime of the scheduler
    let mut open_object = MaybeUninit::uninit();

    // Initialize and run the scheduler
    let mut sched = Scheduler::init(opts, &mut open_object)?;

    if let Some(ctx) = &cgroup_ctx {
        ctx.install_filter(&mut sched)?;
    }

    if let Some(child) = child {
        child.resume()?;
        child.reap_async();
    }
    
    Ok(())
    // sched.run(shutdown)
}
