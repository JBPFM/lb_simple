// SPDX-License-Identifier: GPL-2.0-only

#![allow(unused_imports)]

use libbpf_rs::{MapCore, MapFlags};
use libc::{c_int, pthread_mutex_t};
use std::cell::Cell;

use crate::HELD_LOCKS_MAP;
use crate::bpf_intf::held_lock_info;

thread_local! {
    static LOCK_DEPTH: Cell<u32> = const { Cell::new(0) };
    static LOCK_ADDR: Cell<u64> = const { Cell::new(0) };
    static HOLD_START: Cell<u64> = const { Cell::new(0) };
}

fn get_tid() -> u32 {
    unsafe { libc::syscall(libc::SYS_gettid) as u32 }
}

fn get_now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn update_held_lock(_tid: u32, _mutex: *mut pthread_mutex_t) {}

fn release_held_lock(_tid: u32) {}

redhook::hook! {
    unsafe fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_lock {
        let result = unsafe { redhook::real!(pthread_mutex_lock)(mutex) };
        // if result == 0 {
        //     update_held_lock(get_tid(), mutex);
        // }
        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_unlock {
        let result = unsafe { redhook::real!(pthread_mutex_unlock)(mutex) };
        // if result == 0 {
        //     release_held_lock(get_tid());
        // }
        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_trylock {
        let result = unsafe { redhook::real!(pthread_mutex_trylock)(mutex) };
        // if result == 0 {
        //     update_held_lock(get_tid(), mutex);
        // }
        result
    }
}
