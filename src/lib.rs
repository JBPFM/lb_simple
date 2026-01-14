// SPDX-License-Identifier: GPL-2.0-only
//
// 动态链接库入口，在加载时初始化 eBPF 调度器

mod bpf_skel;
pub use bpf_skel::*;
pub mod bpf_intf;
mod mutex_hook;

use std::mem::MaybeUninit;
use std::sync::OnceLock;

use anyhow::Result;
use libbpf_rs::Link;
use libbpf_rs::MapCore;
use libbpf_rs::MapHandle;
use libbpf_rs::OpenObject;
use log::info;
use scx_utils::scx_ops_attach;
use scx_utils::scx_ops_load;
use scx_utils::scx_ops_open;

const SCHEDULER_NAME: &str = "lb_simple";

fn tick_interval_ns_from_hz() -> u64 {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return 1_000_000;
    }
    1_000_000_000u64 / (hz as u64)
}

// 全局状态，保持 eBPF 程序和 OpenObject 的生命周期
static SCHEDULER_STATE: OnceLock<SchedulerState> = OnceLock::new();

pub(crate) static THREAD_STATE_PTRS_MAP: OnceLock<MapHandle> = OnceLock::new();

struct SchedulerState {
    _link: Link,
    // OpenObject 通过 Box::leak 保持生命周期，不需要显式存储
}

// SAFETY: Link 在内部是线程安全的
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

    // 设置 BPF 参数
    if let Some(rodata) = &mut skel.maps.rodata_data {
        let tick_interval_ns = tick_interval_ns_from_hz();
        let tick_guard_ns = std::env::var("LB_SIMPLE_TICK_GUARD_NS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(200_000);
        let tick_extra_ns = std::env::var("LB_SIMPLE_TICK_EXTRA_NS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let max_boost_hold_ns = std::env::var("LB_SIMPLE_MAX_BOOST_HOLD_NS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5_000_000);

        rodata.tick_interval_ns = tick_interval_ns;
        rodata.tick_guard_ns = tick_guard_ns;
        rodata.tick_extra_ns = tick_extra_ns;
        rodata.max_boost_hold_ns = max_boost_hold_ns;
    }

    // Load the BPF program
    let mut skel = scx_ops_load!(skel, lb_simple_ops, uei)?;

    // Attach the scheduler
    let _link = scx_ops_attach!(skel, lb_simple_ops)?;

    let thread_state_ptrs_map_id = skel.maps.thread_state_ptrs.info()?.info.id;
    let thread_state_ptrs_handle = MapHandle::from_map_id(thread_state_ptrs_map_id)?;
    let _ = THREAD_STATE_PTRS_MAP.set(thread_state_ptrs_handle);

    mutex_hook::register_current_thread();

    info!("{SCHEDULER_NAME} scheduler started via LD_PRELOAD");
    Ok(SchedulerState { _link })
}

impl Drop for SchedulerState {
    fn drop(&mut self) {
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
