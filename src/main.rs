// SPDX-License-Identifier: GPL-2.0-only
//
// Copyright (c) 2024 Andrea Righi <andrea.righi@linux.dev>

mod bpf_skel;
pub use bpf_skel::*;
pub mod bpf_intf;

use std::mem::MaybeUninit;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use libbpf_rs::MapCore;
use libbpf_rs::OpenObject;
use log::info;
use log::warn;
use scx_utils::scx_ops_attach;
use scx_utils::scx_ops_load;
use scx_utils::scx_ops_open;
use scx_utils::uei_exited;
use scx_utils::uei_report;
use libbpf_rs::Link;

const SCHEDULER_NAME: &str = "lb_simple";

/// lb_simple: A simple global weighted vtime scheduler
#[derive(Debug, Parser)]
struct Opts {
    /// Use FIFO scheduling instead of weighted vtime scheduling
    #[clap(short = 'f', long, action = clap::ArgAction::SetTrue)]
    fifo: bool,

    /// Enable verbose output including periodic statistics
    #[clap(short = 'v', long, action = clap::ArgAction::SetTrue)]
    verbose: bool,

    /// Print scheduler statistics every INTERVAL seconds
    #[clap(short = 'i', long, default_value = "2")]
    interval: u64,

    /// Enable debug output
    #[clap(short = 'd', long, action = clap::ArgAction::SetTrue)]
    debug: bool,
}

struct Scheduler<'a> {
    skel: BpfSkel<'a>,
    opts: Opts,
    _link: Link
}

impl<'a> Scheduler<'a> {
    fn init(opts: Opts, open_object: &'a mut MaybeUninit<OpenObject>) -> Result<Self> {
        // Initialize libbpf logging
        let mut skel_builder = BpfSkelBuilder::default();
        skel_builder.obj_builder.debug(opts.debug);

        // Open the BPF skeleton
        let mut skel =
            scx_ops_open!(skel_builder, open_object, lb_simple_ops, None)?;

        // Set BPF variables before loading
        if let Some(rodata) = &mut skel.maps.rodata_data {
            rodata.fifo_sched = opts.fifo;
        }

        // Load the BPF program
        let mut skel = scx_ops_load!(skel, lb_simple_ops, uei)?;

        // Attach the scheduler
        let _link = scx_ops_attach!(skel, lb_simple_ops)?;

        info!("{SCHEDULER_NAME} scheduler started");
        if opts.fifo {
            info!("Scheduling mode: FIFO");
        } else {
            info!("Scheduling mode: Weighted vtime");
        }

        Ok(Self { skel, opts, _link })
    }

    fn read_stats(&mut self) -> Result<(u64, u64)> {
        let mut local_total = 0u64;
        let mut global_total = 0u64;

        // Read per-CPU statistics
        let stats = &self.skel.maps.stats;
        let zero_key = 0u32.to_ne_bytes();
        let one_key = 1u32.to_ne_bytes();

        // Sum up stats from all CPUs
        if let Ok(Some(vec_values)) = stats.lookup_percpu(&zero_key, libbpf_rs::MapFlags::ANY) {
            for value in vec_values.iter() {
                if value.len() >= 8 {
                    local_total += u64::from_ne_bytes(value[0..8].try_into()?);
                }
            }
        }

        if let Ok(Some(vec_values)) = stats.lookup_percpu(&one_key, libbpf_rs::MapFlags::ANY) {
            for value in vec_values.iter() {
                if value.len() >= 8 {
                    global_total += u64::from_ne_bytes(value[0..8].try_into()?);
                }
            }
        }

        Ok((local_total, global_total))
    }

    fn print_stats(&mut self) -> Result<()> {
        let (local, global) = self.read_stats()?;
        let total = local + global;

        if total > 0 {
            info!(
                "Stats: local={} ({:.1}%) global={} ({:.1}%) total={}",
                local,
                (local as f64 / total as f64) * 100.0,
                global,
                (global as f64 / total as f64) * 100.0,
                total
            );
        }

        Ok(())
    }

    fn run(&mut self, shutdown: Arc<AtomicBool>) -> Result<()> {
        let interval = Duration::from_secs(self.opts.interval);

        while !shutdown.load(Ordering::Relaxed) && !uei_exited!(&self.skel, uei) {
            std::thread::sleep(interval);

            if self.opts.verbose {
                if let Err(e) = self.print_stats() {
                    warn!("Failed to print stats: {}", e);
                }
            }
        }

        uei_report!(&self.skel, uei)?;
        Ok(())
    }
}

impl<'a> Drop for Scheduler<'a> {
    fn drop(&mut self) {
        info!("{SCHEDULER_NAME} scheduler stopped");
    }
}

fn main() -> Result<()> {
    // Parse command line arguments
    let opts = Opts::parse();

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

    // Allocate open_object for the lifetime of the scheduler
    let mut open_object = MaybeUninit::uninit();

    // Initialize and run the scheduler
    let mut sched = Scheduler::init(opts, &mut open_object)?;
    sched.run(shutdown)
}
