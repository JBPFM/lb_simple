// SPDX-License-Identifier: GPL-2.0-only
//
// Interpose pthread mutex/cond and back them with a pure spin-yield lock.

use libc::{
    c_int, c_void, pthread_cond_t, pthread_condattr_t, pthread_key_t, pthread_mutex_t,
    pthread_mutexattr_t, timespec,
};
use std::cell::{Cell, UnsafeCell};
use std::cmp::Ordering as CmpOrdering;
use std::hint::spin_loop;
use std::mem::{align_of, size_of};
use std::ptr;
use std::sync::atomic::{fence, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::thread::yield_now;

const SPINPARK_SPIN_TIME: u32 = 256;
const SPINPARK_WAIT_SPIN_TIME: u32 = 4096;

const OBJ_UNINIT: u32 = 0;
const OBJ_INITING: u32 = 1;
const OBJ_READY: u32 = 2;

const YIELD_NONE: u32 = 0;
const YIELD_LOCK_CONTENTION: u32 = 1;
const YIELD_LOCK_HANDOFF: u32 = 2;
const LOCK_STATE_UNLOCKED: u32 = 0;
const LOCK_STATE_LOCKED: u32 = 1;
const LOCK_STATE_QUEUED: u32 = 2;
const HANDOFF_RETRY_MAX: u32 = 1;
const VIP_DSQ_BASE: u64 = 1;
const VIP_DSQ_SLOTS: u32 = 4096;
const VIP_DSQ_LAST: u64 = VIP_DSQ_BASE + (VIP_DSQ_SLOTS as u64) - 1;

const BPF_MAP_UPDATE_ELEM: libc::c_long = 2;
const BPF_MAP_DELETE_ELEM: libc::c_long = 3;

/// Exposed from lib.rs once the scheduler is initialized.
pub(crate) static YIELD_ADDR_MAP_FD: AtomicI32 = AtomicI32::new(-1);

static TLS_CLEANUP_KEY: OnceLock<pthread_key_t> = OnceLock::new();
static NEXT_VIP_SLOT: AtomicU32 = AtomicU32::new(0);
static LOCK_ACQUIRE_CNT: AtomicU64 = AtomicU64::new(0);
static YIELD_CONTENTION_CNT: AtomicU64 = AtomicU64::new(0);
static YIELD_REQUEUE_CNT: AtomicU64 = AtomicU64::new(0);
static YIELD_HANDOFF_CNT: AtomicU64 = AtomicU64::new(0);
static YIELD_FALLBACK_CNT: AtomicU64 = AtomicU64::new(0);
static HANDOFF_TAKEN_CNT: AtomicU64 = AtomicU64::new(0);
static HANDOFF_MISS_CNT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct YieldStatsSnapshot {
    pub lock_acquire: u64,
    pub contention_yield: u64,
    pub requeue_yield: u64,
    pub handoff_yield: u64,
    pub fallback_yield: u64,
    pub handoff_taken: u64,
    pub handoff_miss: u64,
}

pub(crate) fn yield_stats_snapshot() -> YieldStatsSnapshot {
    YieldStatsSnapshot {
        lock_acquire: LOCK_ACQUIRE_CNT.load(Ordering::Relaxed),
        contention_yield: YIELD_CONTENTION_CNT.load(Ordering::Relaxed),
        requeue_yield: YIELD_REQUEUE_CNT.load(Ordering::Relaxed),
        handoff_yield: YIELD_HANDOFF_CNT.load(Ordering::Relaxed),
        fallback_yield: YIELD_FALLBACK_CNT.load(Ordering::Relaxed),
        handoff_taken: HANDOFF_TAKEN_CNT.load(Ordering::Relaxed),
        handoff_miss: HANDOFF_MISS_CNT.load(Ordering::Relaxed),
    }
}

#[repr(C)]
struct LockAs {
    status: AtomicU32,
    lock_ptr: AtomicUsize,
}

#[repr(C)]
struct CondAs {
    status: AtomicU32,
    cond_ptr: AtomicUsize,
}

#[repr(C)]
struct SpinparkLock {
    // 0: unlocked, 1: locked, 2: locked with queued waiters.
    data: AtomicU32,
    // 0: no handoff in progress, 1: unlock yielded for a waiter handoff.
    handoff: AtomicU32,
    // Number of waiters currently parked for this lock.
    waiters: AtomicU32,
    // Lock-dedicated VIP DSQ used by BPF routing and handoff.
    vip_dsq_id: u64,
}

#[repr(C)]
struct SpinparkCond {
    seq: AtomicU32,
    target: AtomicU32,
}

#[repr(C)]
struct TaskYieldInfo {
    reason: u32,
    // seqcount: odd while writing, even when committed.
    r#gen: u32,
    vip_dsq_id: u64,
}

#[repr(C)]
struct YieldAddrEntry {
    user_ptr: u64,
    last_gen: u32,
    bpf_reason: u8,
    pad: [u8; 3],
    vip_dsq_id: u64,
}

#[repr(C)]
struct BpfMapElemAttr {
    map_fd: u32,
    _pad0: u32,
    key: u64,
    value: u64,
    flags: u64,
}

thread_local! {
    static YIELD_INFO: UnsafeCell<TaskYieldInfo> = const { UnsafeCell::new(
        TaskYieldInfo { reason: YIELD_NONE, r#gen: 0, vip_dsq_id: 0 }
    ) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
    static REGISTERED_TID: Cell<u32> = const { Cell::new(0) };
    static CLEANUP_ARMED: Cell<bool> = const { Cell::new(false) };
}

#[inline]
fn current_tid() -> u32 {
    unsafe { libc::syscall(libc::SYS_gettid) as u32 }
}

#[inline]
fn bpf_map_update_elem(fd: i32, key: *const c_void, value: *const c_void) -> i32 {
    let attr = BpfMapElemAttr {
        map_fd: fd as u32,
        _pad0: 0,
        key: key as u64,
        value: value as u64,
        flags: 0,
    };

    unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_MAP_UPDATE_ELEM,
            &attr as *const _ as *const c_void,
            size_of::<BpfMapElemAttr>(),
        ) as i32
    }
}

#[inline]
fn bpf_map_delete_elem(fd: i32, key: *const c_void) -> i32 {
    let attr = BpfMapElemAttr {
        map_fd: fd as u32,
        _pad0: 0,
        key: key as u64,
        value: 0,
        flags: 0,
    };

    unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_MAP_DELETE_ELEM,
            &attr as *const _ as *const c_void,
            size_of::<BpfMapElemAttr>(),
        ) as i32
    }
}

extern "C" fn cleanup_registered_yield_addr(_value: *mut c_void) {
    let fd = YIELD_ADDR_MAP_FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }

    let tid = current_tid();
    let _ = bpf_map_delete_elem(fd, (&tid as *const u32).cast::<c_void>());
}

fn get_cleanup_key() -> Option<pthread_key_t> {
    if let Some(key) = TLS_CLEANUP_KEY.get() {
        return Some(*key);
    }

    let mut new_key: pthread_key_t = 0;
    let rc = unsafe { libc::pthread_key_create(&mut new_key, Some(cleanup_registered_yield_addr)) };
    if rc != 0 {
        return None;
    }

    if TLS_CLEANUP_KEY.set(new_key).is_err() {
        unsafe { libc::pthread_key_delete(new_key) };
    }

    TLS_CLEANUP_KEY.get().copied()
}

fn arm_thread_cleanup() {
    CLEANUP_ARMED.with(|armed| {
        if armed.get() {
            return;
        }

        let Some(key) = get_cleanup_key() else {
            return;
        };

        let marker = std::ptr::NonNull::<u8>::dangling()
            .as_ptr()
            .cast::<c_void>();
        let rc = unsafe { libc::pthread_setspecific(key, marker) };
        if rc == 0 {
            armed.set(true);
        }
    });
}

fn ensure_registered() {
    let fd = YIELD_ADDR_MAP_FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }

    let tid = current_tid();
    REGISTERED.with(|registered| {
        REGISTERED_TID.with(|registered_tid| {
            if registered.get() && registered_tid.get() == tid {
                return;
            }

            YIELD_INFO.with(|info| {
                let entry = YieldAddrEntry {
                    user_ptr: info.get() as u64,
                    last_gen: 0,
                    bpf_reason: YIELD_NONE as u8,
                    pad: [0; 3],
                    vip_dsq_id: 0,
                };

                let rc = bpf_map_update_elem(
                    fd,
                    (&tid as *const u32).cast::<c_void>(),
                    (&entry as *const YieldAddrEntry).cast::<c_void>(),
                );
                if rc == 0 {
                    registered.set(true);
                    registered_tid.set(tid);
                    arm_thread_cleanup();
                } else {
                    registered.set(false);
                }
            });
        });
    });
}

#[inline]
fn is_registered_fast() -> bool {
    if REGISTERED.with(|registered| registered.get()) {
        return true;
    }
    YIELD_ADDR_MAP_FD.load(Ordering::Acquire) >= 0 && REGISTERED.with(|registered| registered.get())
}

#[inline]
fn set_yield_info(reason: u32, vip_dsq_id: u64) {
    YIELD_INFO.with(|info| {
        let p = info.get();
        unsafe {
            // Begin seqcount write section (odd gen).
            let write_gen = (*p).r#gen.wrapping_add(1) | 1;
            (*p).r#gen = write_gen;
            fence(Ordering::Release);

            (*p).reason = reason;
            (*p).vip_dsq_id = vip_dsq_id;

            // Commit seqcount write section (even gen).
            fence(Ordering::Release);
            (*p).r#gen = write_gen.wrapping_add(1);
        }
    });
}

#[inline]
fn alloc_vip_dsq_id() -> u64 {
    let slot = NEXT_VIP_SLOT.fetch_add(1, Ordering::Relaxed);
    if slot >= VIP_DSQ_SLOTS {
        return 0;
    }
    VIP_DSQ_BASE + slot as u64
}

#[inline]
fn strong_handoff_enabled(lock_ref: &SpinparkLock) -> bool {
    let id = lock_ref.vip_dsq_id;
    id >= VIP_DSQ_BASE && id <= VIP_DSQ_LAST
}

#[inline]
fn mark_contended(lock_ref: &SpinparkLock) {
    let _ = lock_ref.data.compare_exchange(
        LOCK_STATE_LOCKED,
        LOCK_STATE_QUEUED,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

#[inline]
fn dec_waiters(lock_ref: &SpinparkLock) -> u32 {
    loop {
        let cur = lock_ref.waiters.load(Ordering::Acquire);
        if cur == 0 {
            return 0;
        }

        if lock_ref
            .waiters
            .compare_exchange_weak(cur, cur - 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return cur - 1;
        }
    }
}

#[inline]
fn settle_post_acquire(lock_ref: &SpinparkLock, waiter_accounted: &mut bool) {
    if !*waiter_accounted {
        return;
    }

    let remaining = dec_waiters(lock_ref);
    if remaining > 0 {
        lock_ref.data.store(LOCK_STATE_QUEUED, Ordering::Release);
    } else {
        lock_ref.data.store(LOCK_STATE_LOCKED, Ordering::Release);
    }
    *waiter_accounted = false;
}

#[inline]
fn try_consume_handoff(lock_ref: &SpinparkLock, waiter_accounted: &mut bool) -> bool {
    if lock_ref
        .handoff
        .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }

    if *waiter_accounted {
        settle_post_acquire(lock_ref, waiter_accounted);
    } else if lock_ref.waiters.load(Ordering::Acquire) == 0 {
        lock_ref.data.store(LOCK_STATE_LOCKED, Ordering::Release);
    } else {
        lock_ref.data.store(LOCK_STATE_QUEUED, Ordering::Release);
    }

    HANDOFF_TAKEN_CNT.fetch_add(1, Ordering::Relaxed);
    LOCK_ACQUIRE_CNT.fetch_add(1, Ordering::Relaxed);
    true
}

#[inline]
fn mutex_layout_ok() -> bool {
    size_of::<pthread_mutex_t>() >= size_of::<LockAs>()
        && align_of::<pthread_mutex_t>() >= align_of::<LockAs>()
}

#[inline]
fn cond_layout_ok() -> bool {
    size_of::<pthread_cond_t>() >= size_of::<CondAs>()
        && align_of::<pthread_cond_t>() >= align_of::<CondAs>()
}

#[inline]
unsafe fn lock_as(mutex: *mut pthread_mutex_t) -> *mut LockAs {
    mutex.cast::<LockAs>()
}

#[inline]
unsafe fn cond_as(cond: *mut pthread_cond_t) -> *mut CondAs {
    cond.cast::<CondAs>()
}

#[inline]
fn spin_yield_backoff(counter: &mut u32) {
    *counter += 1;
    if *counter >= SPINPARK_SPIN_TIME {
        yield_now();
        *counter = 0;
    } else {
        spin_loop();
    }
}

fn get_or_init_lock(mutex: *mut pthread_mutex_t, create: bool) -> Result<*mut SpinparkLock, c_int> {
    if mutex.is_null() || !mutex_layout_ok() {
        return Err(libc::EINVAL);
    }

    let meta = unsafe { lock_as(mutex) };
    loop {
        let st = unsafe { (*meta).status.load(Ordering::Acquire) };
        match st {
            OBJ_READY => {
                let ptr = unsafe { (*meta).lock_ptr.load(Ordering::Acquire) } as *mut SpinparkLock;
                if ptr.is_null() {
                    unsafe { (*meta).status.store(OBJ_UNINIT, Ordering::Release) };
                    continue;
                }
                return Ok(ptr);
            }
            OBJ_UNINIT if create => {
                let won = unsafe {
                    (*meta).status.compare_exchange(
                        OBJ_UNINIT,
                        OBJ_INITING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                };
                if won.is_ok() {
                    let lock = Box::new(SpinparkLock {
                        data: AtomicU32::new(LOCK_STATE_UNLOCKED),
                        handoff: AtomicU32::new(0),
                        waiters: AtomicU32::new(0),
                        vip_dsq_id: alloc_vip_dsq_id(),
                    });
                    let lock_ptr = Box::into_raw(lock);
                    unsafe {
                        (*meta).lock_ptr.store(lock_ptr as usize, Ordering::Release);
                        (*meta).status.store(OBJ_READY, Ordering::Release);
                    }
                    return Ok(lock_ptr);
                }
            }
            OBJ_UNINIT => return Err(libc::EINVAL),
            OBJ_INITING => spin_loop(),
            _ => return Err(libc::EINVAL),
        }
    }
}

fn get_or_init_cond(cond: *mut pthread_cond_t, create: bool) -> Result<*mut SpinparkCond, c_int> {
    if cond.is_null() || !cond_layout_ok() {
        return Err(libc::EINVAL);
    }

    let meta = unsafe { cond_as(cond) };
    loop {
        let st = unsafe { (*meta).status.load(Ordering::Acquire) };
        match st {
            OBJ_READY => {
                let ptr = unsafe { (*meta).cond_ptr.load(Ordering::Acquire) } as *mut SpinparkCond;
                if ptr.is_null() {
                    unsafe { (*meta).status.store(OBJ_UNINIT, Ordering::Release) };
                    continue;
                }
                return Ok(ptr);
            }
            OBJ_UNINIT if create => {
                let won = unsafe {
                    (*meta).status.compare_exchange(
                        OBJ_UNINIT,
                        OBJ_INITING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                };
                if won.is_ok() {
                    let cond_obj = Box::new(SpinparkCond {
                        seq: AtomicU32::new(0),
                        target: AtomicU32::new(0),
                    });
                    let cond_ptr = Box::into_raw(cond_obj);
                    unsafe {
                        (*meta).cond_ptr.store(cond_ptr as usize, Ordering::Release);
                        (*meta).status.store(OBJ_READY, Ordering::Release);
                    }
                    return Ok(cond_ptr);
                }
            }
            OBJ_UNINIT => return Err(libc::EINVAL),
            OBJ_INITING => spin_loop(),
            _ => return Err(libc::EINVAL),
        }
    }
}

fn interpose_lock_init(mutex: *mut pthread_mutex_t, force_reinit: bool) -> c_int {
    if mutex.is_null() || !mutex_layout_ok() {
        return libc::EINVAL;
    }

    let meta = unsafe { lock_as(mutex) };
    if force_reinit {
        let old_ptr = unsafe { (*meta).lock_ptr.swap(0, Ordering::AcqRel) } as *mut SpinparkLock;
        if !old_ptr.is_null() {
            unsafe { drop(Box::from_raw(old_ptr)) };
        }
        unsafe { (*meta).status.store(OBJ_UNINIT, Ordering::Release) };
    }

    match get_or_init_lock(mutex, true) {
        Ok(_) => 0,
        Err(e) => e,
    }
}

fn interpose_cond_init(cond: *mut pthread_cond_t, force_reinit: bool) -> c_int {
    if cond.is_null() || !cond_layout_ok() {
        return libc::EINVAL;
    }

    let meta = unsafe { cond_as(cond) };
    if force_reinit {
        let old_ptr = unsafe { (*meta).cond_ptr.swap(0, Ordering::AcqRel) } as *mut SpinparkCond;
        if !old_ptr.is_null() {
            unsafe { drop(Box::from_raw(old_ptr)) };
        }
        unsafe { (*meta).status.store(OBJ_UNINIT, Ordering::Release) };
    }

    match get_or_init_cond(cond, true) {
        Ok(_) => 0,
        Err(e) => e,
    }
}

fn interpose_lock_destroy(mutex: *mut pthread_mutex_t) -> c_int {
    if mutex.is_null() || !mutex_layout_ok() {
        return libc::EINVAL;
    }

    let meta = unsafe { lock_as(mutex) };
    let ptr = unsafe { (*meta).lock_ptr.swap(0, Ordering::AcqRel) } as *mut SpinparkLock;
    unsafe { (*meta).status.store(OBJ_UNINIT, Ordering::Release) };
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)) };
    }
    0
}

fn interpose_cond_destroy(cond: *mut pthread_cond_t) -> c_int {
    if cond.is_null() || !cond_layout_ok() {
        return libc::EINVAL;
    }

    let meta = unsafe { cond_as(cond) };
    let ptr = unsafe { (*meta).cond_ptr.swap(0, Ordering::AcqRel) } as *mut SpinparkCond;
    unsafe { (*meta).status.store(OBJ_UNINIT, Ordering::Release) };
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)) };
    }
    0
}

#[inline]
fn spinpark_trylock(lock: *mut SpinparkLock) -> c_int {
    let lock_ref = unsafe { &*lock };
    if lock_ref
        .data
        .compare_exchange(
            LOCK_STATE_UNLOCKED,
            LOCK_STATE_LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        )
        .is_ok()
    {
        LOCK_ACQUIRE_CNT.fetch_add(1, Ordering::Relaxed);
        return 0;
    }
    libc::EBUSY
}

#[inline]
fn spinpark_lock(lock: *mut SpinparkLock) {
    let lock_ref = unsafe { &*lock };
    let mut spin_count = 0u32;
    let mut waiter_accounted = false;

    loop {
        // TTAS: only attempt RMW when lock looks free.
        if lock_ref.data.load(Ordering::Relaxed) == LOCK_STATE_UNLOCKED
            && lock_ref
                .data
                .compare_exchange_weak(
                    LOCK_STATE_UNLOCKED,
                    LOCK_STATE_LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            settle_post_acquire(lock_ref, &mut waiter_accounted);
            LOCK_ACQUIRE_CNT.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if waiter_accounted
            && strong_handoff_enabled(lock_ref)
            && lock_ref.data.load(Ordering::Acquire) == LOCK_STATE_QUEUED
            && try_consume_handoff(lock_ref, &mut waiter_accounted)
        {
            return;
        }

        spin_count = spin_count.wrapping_add(1);
        let spin_limit = if waiter_accounted {
            SPINPARK_WAIT_SPIN_TIME
        } else {
            SPINPARK_SPIN_TIME
        };
        if spin_count < spin_limit {
            spin_loop();
            continue;
        }

        if !strong_handoff_enabled(lock_ref) {
            // Fallback mode for locks without a dedicated VIP DSQ.
            YIELD_FALLBACK_CNT.fetch_add(1, Ordering::Relaxed);
            unsafe { libc::sched_yield() };
            spin_count = 0;
            continue;
        }

        // Mark lock as queued before parking.
        mark_contended(lock_ref);

        // Declare waiter once per lock attempt; keep it accounted across retries.
        let first_park = !waiter_accounted;
        if !waiter_accounted {
            lock_ref.waiters.fetch_add(1, Ordering::AcqRel);
            waiter_accounted = true;
        }
        if lock_ref.data.load(Ordering::Acquire) == LOCK_STATE_UNLOCKED
            && lock_ref
                .data
                .compare_exchange(
                    LOCK_STATE_UNLOCKED,
                    LOCK_STATE_LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            settle_post_acquire(lock_ref, &mut waiter_accounted);
            LOCK_ACQUIRE_CNT.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if try_consume_handoff(lock_ref, &mut waiter_accounted) {
            return;
        }

        if !is_registered_fast() {
            ensure_registered();
        }
        set_yield_info(YIELD_LOCK_CONTENTION, lock_ref.vip_dsq_id);
        if first_park {
            YIELD_CONTENTION_CNT.fetch_add(1, Ordering::Relaxed);
        } else {
            YIELD_REQUEUE_CNT.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { libc::sched_yield() };

        if try_consume_handoff(lock_ref, &mut waiter_accounted) {
            return;
        }
        spin_count = 0;
    }
}

#[inline]
fn spinpark_unlock(lock: *mut SpinparkLock) -> c_int {
    let lock_ref = unsafe { &*lock };
    let state = lock_ref.data.load(Ordering::Acquire);

    if state == LOCK_STATE_LOCKED {
        lock_ref.handoff.store(0, Ordering::Relaxed);
        lock_ref.data.store(LOCK_STATE_UNLOCKED, Ordering::Release);
        return 0;
    }

    if state == LOCK_STATE_QUEUED
        && strong_handoff_enabled(lock_ref)
        && lock_ref.waiters.load(Ordering::Acquire) != 0
    {
        if !is_registered_fast() {
            ensure_registered();
        }
        for _ in 0..HANDOFF_RETRY_MAX {
            lock_ref.handoff.store(1, Ordering::Release);
            set_yield_info(YIELD_LOCK_HANDOFF, lock_ref.vip_dsq_id);
            YIELD_HANDOFF_CNT.fetch_add(1, Ordering::Relaxed);
            unsafe { libc::sched_yield() };

            /*
             * compare_exchange() succeeds only when value is still 1, meaning
             * no waiter consumed this handoff.
             */
            if lock_ref
                .handoff
                .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return 0;
            }

            if lock_ref.waiters.load(Ordering::Acquire) == 0 {
                break;
            }
        }

        HANDOFF_MISS_CNT.fetch_add(1, Ordering::Relaxed);
        lock_ref.data.store(LOCK_STATE_UNLOCKED, Ordering::Release);
        return 0;
    }

    lock_ref.handoff.store(0, Ordering::Relaxed);
    lock_ref.data.store(LOCK_STATE_UNLOCKED, Ordering::Release);
    0
}

#[inline]
fn timespec_cmp(a: &timespec, b: &timespec) -> CmpOrdering {
    if a.tv_sec < b.tv_sec {
        return CmpOrdering::Less;
    }
    if a.tv_sec > b.tv_sec {
        return CmpOrdering::Greater;
    }
    a.tv_nsec.cmp(&b.tv_nsec)
}

fn spinpark_cond_wait(
    cond: *mut SpinparkCond,
    lock: *mut SpinparkLock,
    abstime: *const timespec,
) -> c_int {
    let cond_ref = unsafe { &*cond };
    let target = cond_ref
        .target
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let mut seq = cond_ref.seq.load(Ordering::Acquire);

    let unlock_rc = spinpark_unlock(lock);
    if unlock_rc != 0 {
        return unlock_rc;
    }

    if abstime.is_null() {
        let mut backoff = 0;
        while target > seq {
            spin_yield_backoff(&mut backoff);
            seq = cond_ref.seq.load(Ordering::Acquire);
        }
        spinpark_lock(lock);
        return 0;
    }

    let mut backoff = 0;
    while target > seq {
        let mut now = timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let _ = unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
        let abs = unsafe { &*abstime };
        if timespec_cmp(&now, abs) != CmpOrdering::Less {
            spinpark_lock(lock);
            return libc::ETIMEDOUT;
        }

        spin_yield_backoff(&mut backoff);
        seq = cond_ref.seq.load(Ordering::Acquire);
    }

    spinpark_lock(lock);
    0
}

#[inline]
fn spinpark_cond_signal(cond: *mut SpinparkCond) -> c_int {
    let cond_ref = unsafe { &*cond };
    let _ = cond_ref.seq.fetch_add(1, Ordering::Release);
    0
}

#[inline]
fn spinpark_cond_broadcast(cond: *mut SpinparkCond) -> c_int {
    let cond_ref = unsafe { &*cond };
    let target = cond_ref.target.load(Ordering::Acquire);
    cond_ref.seq.store(target, Ordering::Release);
    0
}

redhook::hook! {
    unsafe fn pthread_mutex_init(
        mutex: *mut pthread_mutex_t,
        attr: *const pthread_mutexattr_t
    ) -> c_int => my_pthread_mutex_init {
        let _ = attr;
        interpose_lock_init(mutex, true)
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_destroy(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_destroy {
        interpose_lock_destroy(mutex)
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_lock {
        let lock = match get_or_init_lock(mutex, true) {
            Ok(l) => l,
            Err(e) => return e,
        };
        spinpark_lock(lock);
        0
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_trylock {
        let lock = match get_or_init_lock(mutex, true) {
            Ok(l) => l,
            Err(e) => return e,
        };
        spinpark_trylock(lock)
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_unlock {
        let lock = match get_or_init_lock(mutex, false) {
            Ok(l) => l,
            Err(e) => return e,
        };
        spinpark_unlock(lock)
    }
}

redhook::hook! {
    unsafe fn pthread_cond_init(
        cond: *mut pthread_cond_t,
        attr: *const pthread_condattr_t
    ) -> c_int => my_pthread_cond_init {
        let _ = attr;
        interpose_cond_init(cond, true)
    }
}

redhook::hook! {
    unsafe fn pthread_cond_destroy(cond: *mut pthread_cond_t) -> c_int => my_pthread_cond_destroy {
        interpose_cond_destroy(cond)
    }
}

redhook::hook! {
    unsafe fn pthread_cond_wait(
        cond: *mut pthread_cond_t,
        mutex: *mut pthread_mutex_t
    ) -> c_int => my_pthread_cond_wait {
        let cond_obj = match get_or_init_cond(cond, true) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let lock_obj = match get_or_init_lock(mutex, true) {
            Ok(l) => l,
            Err(e) => return e,
        };

        spinpark_cond_wait(cond_obj, lock_obj, ptr::null::<timespec>())
    }
}

redhook::hook! {
    unsafe fn pthread_cond_timedwait(
        cond: *mut pthread_cond_t,
        mutex: *mut pthread_mutex_t,
        abstime: *const timespec
    ) -> c_int => my_pthread_cond_timedwait {
        if abstime.is_null() {
            return libc::EINVAL;
        }

        let cond_obj = match get_or_init_cond(cond, true) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let lock_obj = match get_or_init_lock(mutex, true) {
            Ok(l) => l,
            Err(e) => return e,
        };

        spinpark_cond_wait(cond_obj, lock_obj, abstime)
    }
}

redhook::hook! {
    unsafe fn pthread_cond_signal(cond: *mut pthread_cond_t) -> c_int => my_pthread_cond_signal {
        let cond_obj = match get_or_init_cond(cond, true) {
            Ok(c) => c,
            Err(e) => return e,
        };
        spinpark_cond_signal(cond_obj)
    }
}

redhook::hook! {
    unsafe fn pthread_cond_broadcast(cond: *mut pthread_cond_t) -> c_int => my_pthread_cond_broadcast {
        let cond_obj = match get_or_init_cond(cond, true) {
            Ok(c) => c,
            Err(e) => return e,
        };
        spinpark_cond_broadcast(cond_obj)
    }
}
