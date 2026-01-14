// SPDX-License-Identifier: GPL-2.0-only

//! Mutex hook module for tracking user-space lock state.
//!
//! Provides pthread function hooks to track mutex lock depth per thread.
//! Lock state is shared with BPF programs via a map, enabling kernel-side
//! awareness of user-space lock holding status.
//!
//! ```text
//! ┌─────────────────┐     ┌──────────────────────┐     ┌─────────────┐
//! │  User Thread    │────▶│  THREAD_STATE_PTRS   │◀────│  BPF Prog   │
//! │  (lock depth)   │     │  (tid -> state ptr)  │     │  (reader)   │
//! └─────────────────┘     └──────────────────────┘     └─────────────┘
//! ```

#![allow(unused_imports)]

use libbpf_rs::{MapCore, MapFlags};
use libc::{c_int, c_void, pthread_attr_t, pthread_mutex_t, pthread_t};
use std::cell::{Cell, RefCell};

use crate::THREAD_STATE_PTRS_MAP;

/// Per-thread lock state shared with BPF programs via raw pointer.
#[repr(C)]
#[derive(Copy, Clone)]
struct UserLockState {
    depth: u32,
}

impl UserLockState {
    const fn new() -> Self {
        Self { depth: 0 }
    }

    #[inline]
    fn acquire(&mut self) {
        self.depth = self.depth.saturating_add(1);
    }

    #[inline]
    fn release(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
        }
    }
}

thread_local! {
    static LOCK_STATE: RefCell<UserLockState> = const { RefCell::new(UserLockState::new()) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

#[inline]
fn get_tid() -> u32 {
    unsafe { libc::syscall(libc::SYS_gettid) as u32 }
}

fn register_thread_state_ptr() -> bool {
    let Some(map) = THREAD_STATE_PTRS_MAP.get() else {
        return false;
    };

    let tid = get_tid();
    let ptr = LOCK_STATE.with(|st| st.as_ptr() as u64);

    map.update(&tid.to_ne_bytes(), &ptr.to_ne_bytes(), MapFlags::ANY)
        .is_ok()
}

fn unregister_thread_state_ptr() {
    if let Some(map) = THREAD_STATE_PTRS_MAP.get() {
        let _ = map.delete(&get_tid().to_ne_bytes());
    }
}

fn ensure_registered() {
    REGISTERED.with(|registered| {
        if !registered.get() && register_thread_state_ptr() {
            registered.set(true);
        }
    });
}

pub(crate) fn register_current_thread() {
    ensure_registered();
}

#[inline]
fn on_lock_acquired() {
    ensure_registered();
    LOCK_STATE.with(|st| st.borrow_mut().acquire());
}

#[inline]
fn on_unlock_released() {
    LOCK_STATE.with(|st| st.borrow_mut().release());
}

struct PthreadStartArgs {
    start: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
}

extern "C" fn pthread_start_trampoline(arg: *mut c_void) -> *mut c_void {
    ensure_registered();
    let args = unsafe { Box::from_raw(arg as *mut PthreadStartArgs) };
    (args.start)(args.arg)
}

redhook::hook! {
    unsafe fn pthread_create(
        thread: *mut pthread_t,
        attr: *const pthread_attr_t,
        start_routine: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void
    ) -> c_int => my_pthread_create {
        let wrapped = Box::new(PthreadStartArgs { start: start_routine, arg });
        let wrapped_ptr = Box::into_raw(wrapped) as *mut c_void;
        unsafe { redhook::real!(pthread_create)(thread, attr, pthread_start_trampoline, wrapped_ptr) }
    }
}

redhook::hook! {
    unsafe fn pthread_exit(value_ptr: *mut c_void) -> ! => my_pthread_exit {
        unregister_thread_state_ptr();
        REGISTERED.with(|r| r.set(false));
        unsafe { redhook::real!(pthread_exit)(value_ptr) }
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_lock {
        let result = unsafe { redhook::real!(pthread_mutex_lock)(mutex) };
        if result == 0 {
            on_lock_acquired();
        }
        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_unlock {
        let result = unsafe { redhook::real!(pthread_mutex_unlock)(mutex) };
        if result == 0 {
            on_unlock_released();
        }
        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_trylock {
        let result = unsafe { redhook::real!(pthread_mutex_trylock)(mutex) };
        if result == 0 {
            on_lock_acquired();
        }
        result
    }
}
