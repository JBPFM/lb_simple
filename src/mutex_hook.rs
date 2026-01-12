// SPDX-License-Identifier: GPL-2.0-only

use libbpf_rs::{MapCore, MapFlags};
use libc::{c_int, pthread_mutex_t};

use crate::HELD_LOCKS_MAP;
use crate::bpf_intf::held_lock_info;

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

fn update_held_lock(tid: u32, mutex: *mut pthread_mutex_t) {
    let Some(map) = HELD_LOCKS_MAP.get() else {
        return;
    };

    let key = tid.to_ne_bytes();
    let existing = map.lookup(&key, MapFlags::ANY).ok().flatten();

    let info = if let Some(data) = existing {
        if data.len() >= std::mem::size_of::<held_lock_info>() {
            let mut info: held_lock_info = unsafe { std::ptr::read(data.as_ptr() as *const _) };
            info.depth = info.depth.saturating_add(1);
            info
        } else {
            held_lock_info {
                lock_addr: mutex as u64,
                hold_start_ns: get_now_ns(),
                depth: 1,
                pad: 0,
            }
        }
    } else {
        held_lock_info {
            lock_addr: mutex as u64,
            hold_start_ns: get_now_ns(),
            depth: 1,
            pad: 0,
        }
    };

    let value = unsafe {
        std::slice::from_raw_parts(
            &info as *const _ as *const u8,
            std::mem::size_of::<held_lock_info>(),
        )
    };
    let _ = map.update(&key, value, MapFlags::ANY);
}

fn release_held_lock(tid: u32) {
    let Some(map) = HELD_LOCKS_MAP.get() else {
        return;
    };

    let key = tid.to_ne_bytes();
    let existing = map.lookup(&key, MapFlags::ANY).ok().flatten();

    if let Some(data) = existing {
        if data.len() >= std::mem::size_of::<held_lock_info>() {
            let mut info: held_lock_info = unsafe { std::ptr::read(data.as_ptr() as *const _) };
            info.depth = info.depth.saturating_sub(1);

            if info.depth == 0 {
                let _ = map.delete(&key);
            } else {
                let value = unsafe {
                    std::slice::from_raw_parts(
                        &info as *const _ as *const u8,
                        std::mem::size_of::<held_lock_info>(),
                    )
                };
                let _ = map.update(&key, value, MapFlags::ANY);
            }
        }
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_lock {
        let result = unsafe { redhook::real!(pthread_mutex_lock)(mutex) };

        if result == 0 {
            update_held_lock(get_tid(), mutex);
        }

        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_unlock {
        let result = unsafe { redhook::real!(pthread_mutex_unlock)(mutex) };

        if result == 0 {
            release_held_lock(get_tid());
        }

        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_trylock {
        let result = unsafe { redhook::real!(pthread_mutex_trylock)(mutex) };

        if result == 0 {
            update_held_lock(get_tid(), mutex);
        }

        result
    }
}
