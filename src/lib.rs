// SPDX-License-Identifier: GPL-2.0-only
//
// 动态链接库入口，在加载时初始化 eBPF 调度器

mod bpf_skel;
pub use bpf_skel::*;
pub mod bpf_intf;
mod mutex_hook;

use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use anyhow::Result;
use libbpf_rs::Link;
use libbpf_rs::OpenObject;
use log::info;
use scx_utils::scx_ops_attach;
use scx_utils::scx_ops_load;
use scx_utils::scx_ops_open;

use crate::mutex_hook::{YIELD_ADDR_MAP_FD, yield_stats_snapshot};

const SCHEDULER_NAME: &str = "lb_simple";

// 全局状态，保持 eBPF 程序和 OpenObject 的生命周期
static SCHEDULER_STATE: OnceLock<SchedulerState> = OnceLock::new();

struct SchedulerState {
    // Keep link and loaded skel alive for the entire process lifetime.
    _link: Option<Link>,
    _skel: Option<BpfSkel<'static>>,
}

// SAFETY: BpfSkel/Link are internally thread-safe for this usage.
unsafe impl Send for SchedulerState {}
unsafe impl Sync for SchedulerState {}

fn init_scheduler(debug: bool) -> Result<SchedulerState> {
    let mut skel_builder = BpfSkelBuilder::default();
    skel_builder.obj_builder.debug(debug);

    // 使用 Box::leak 来保持 OpenObject 的生命周期
    let open_object: &'static mut MaybeUninit<OpenObject> =
        Box::leak(Box::new(MaybeUninit::uninit()));

    // Open the BPF skeleton
    let mut skel = scx_ops_open!(skel_builder, open_object, lb_simple_ops, None)?;

    // Load the BPF program
    let mut skel = scx_ops_load!(skel, lb_simple_ops, uei)?;

    // Expose map FD to mutex hook code.
    let map_fd = skel.maps.yield_addr_map.as_fd().as_raw_fd();
    YIELD_ADDR_MAP_FD.store(map_fd, Ordering::Release);

    // Attach the scheduler
    let link = scx_ops_attach!(skel, lb_simple_ops)?;

    info!("{SCHEDULER_NAME} scheduler started via LD_PRELOAD");
    Ok(SchedulerState {
        _link: Some(link),
        _skel: Some(skel),
    })
}

impl Drop for SchedulerState {
    fn drop(&mut self) {
        let stats = yield_stats_snapshot();
        info!(
            "yield stats: lock_acquire={} contention_yield={} requeue_yield={} handoff_yield={} fallback_yield={} handoff_taken={} handoff_miss={}",
            stats.lock_acquire,
            stats.contention_yield,
            stats.requeue_yield,
            stats.handoff_yield,
            stats.fallback_yield,
            stats.handoff_taken,
            stats.handoff_miss
        );
        eprintln!(
            "[lb_simple] yield stats: lock_acquire={} contention_yield={} requeue_yield={} handoff_yield={} fallback_yield={} handoff_taken={} handoff_miss={}",
            stats.lock_acquire,
            stats.contention_yield,
            stats.requeue_yield,
            stats.handoff_yield,
            stats.fallback_yield,
            stats.handoff_taken,
            stats.handoff_miss
        );

        // Prevent late TLS destructors from using a stale map fd.
        YIELD_ADDR_MAP_FD.store(-1, Ordering::Release);

        // Drop link first, then skeleton.
        let _ = self._link.take();
        let _ = self._skel.take();

        info!("{SCHEDULER_NAME} scheduler stopped");
    }
}

/// 初始化 eBPF 调度器
fn init_ebpf() {
    // 初始化日志
    let _ = simplelog::TermLogger::init(
        simplelog::LevelFilter::Info,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Auto,
    );

    // 初始化调度器（只执行一次）
    let _ = SCHEDULER_STATE.get_or_init(|| match init_scheduler(false) {
        Ok(state) => {
            eprintln!("[lb_simple] eBPF scheduler loaded successfully");
            state
        }
        Err(e) => {
            eprintln!("[lb_simple] Failed to load eBPF scheduler: {}", e);
            panic!("eBPF initialization failed");
        }
    });
}

// 库加载时的构造函数
#[unsafe(link_section = ".init_array")]
#[used]
static INIT: extern "C" fn() = {
    extern "C" fn init() {
        init_ebpf();
    }
    init
};

// 库卸载/进程退出时打印一次统计，避免 static OnceLock 不触发 Drop 导致无输出。
#[unsafe(link_section = ".fini_array")]
#[used]
static FINI: extern "C" fn() = {
    extern "C" fn fini() {
        let stats = yield_stats_snapshot();
        eprintln!(
            "[lb_simple] yield stats: lock_acquire={} contention_yield={} requeue_yield={} handoff_yield={} fallback_yield={} handoff_taken={} handoff_miss={}",
            stats.lock_acquire,
            stats.contention_yield,
            stats.requeue_yield,
            stats.handoff_yield,
            stats.fallback_yield,
            stats.handoff_taken,
            stats.handoff_miss
        );
    }
    fini
};
