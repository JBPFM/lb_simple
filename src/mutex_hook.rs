// SPDX-License-Identifier: GPL-2.0-only
//
// pthread_mutex_lock 和 pthread_mutex_unlock 的 redhook 实现

use libc::{c_int, pthread_mutex_t};

redhook::hook! {
    unsafe fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_lock {
        // 在调用原始函数之前的处理
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        eprintln!("[HOOK] pthread_mutex_lock called, tid={}, mutex={:p}", tid, mutex);
        
        // 调用原始的 pthread_mutex_lock
        let result = unsafe { redhook::real!(pthread_mutex_lock)(mutex) };
        
        // 在调用原始函数之后的处理
        eprintln!("[HOOK] pthread_mutex_lock returned {}, tid={}", result, tid);
        
        result
    }
}

redhook::hook! {
    unsafe fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int => my_pthread_mutex_unlock {
        // 在调用原始函数之前的处理
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        eprintln!("[HOOK] pthread_mutex_unlock called, tid={}, mutex={:p}", tid, mutex);
        
        // 调用原始的 pthread_mutex_unlock
        let result = unsafe { redhook::real!(pthread_mutex_unlock)(mutex) };
        
        // 在调用原始函数之后的处理
        eprintln!("[HOOK] pthread_mutex_unlock returned {}, tid={}", result, tid);
        
        result
    }
}
