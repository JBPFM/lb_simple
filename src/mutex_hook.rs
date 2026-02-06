// SPDX-License-Identifier: GPL-2.0-only
//
// Interpose pthread mutex/cond and back them with a pure spin-yield lock.

use libc::{
    c_int, pthread_cond_t, pthread_condattr_t, pthread_mutex_t, pthread_mutexattr_t, timespec,
};
use std::cmp::Ordering as CmpOrdering;
use std::hint::spin_loop;
use std::mem::{align_of, size_of};
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::thread::yield_now;

const SPINPARK_SPIN_TIME: u32 = 2700;

const OBJ_UNINIT: u32 = 0;
const OBJ_INITING: u32 = 1;
const OBJ_READY: u32 = 2;

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
    // 0: unlocked, 1: locked.
    data: AtomicU32,
}

#[repr(C)]
struct SpinparkCond {
    seq: AtomicU32,
    target: AtomicU32,
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
                        data: AtomicU32::new(0),
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
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        return 0;
    }
    libc::EBUSY
}

#[inline]
fn spinpark_lock(lock: *mut SpinparkLock) {
    let lock_ref = unsafe { &*lock };
    let mut backoff = 0;
    loop {
        if lock_ref
            .data
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        spin_yield_backoff(&mut backoff);
    }
}

#[inline]
fn spinpark_unlock(lock: *mut SpinparkLock) -> c_int {
    let lock_ref = unsafe { &*lock };
    if lock_ref
        .data
        .compare_exchange(1, 0, Ordering::Release, Ordering::Relaxed)
        .is_err()
    {
        return libc::EPERM;
    }
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
