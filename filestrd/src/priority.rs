//! Lower the CPU and IO scheduling priority of the *calling thread* (Linux,
//! per-task). Used as the `on_thread_start` hook of the dedicated blob-store
//! runtime, so that share hashing — and blob-store IO generally — yields to the
//! user's interactive foreground work, while the daemon's control socket,
//! network endpoint and search routing keep running at normal priority.
//!
//! Best-effort: failures are ignored. Non-Linux is a no-op.

/// Apply a low CPU nice value and a low best-effort IO priority to the calling
/// thread. On Linux nice and IO priority are per-task, so this only affects the
/// thread that runs it (the blob-store runtime's threads).
#[cfg(target_os = "linux")]
pub fn lower_current_thread() {
    // CPU: nice this thread to the most generous value (who=0 is the caller).
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    // IO: best-effort class, lowest level — yields to normal-priority IO but,
    // unlike the idle class, still makes progress under disk contention.
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_BE: libc::c_long = 2;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    const LOWEST_LEVEL: libc::c_long = 7;
    let ioprio = (IOPRIO_CLASS_BE << IOPRIO_CLASS_SHIFT) | LOWEST_LEVEL;
    unsafe {
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, ioprio);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn lower_current_thread() {}
