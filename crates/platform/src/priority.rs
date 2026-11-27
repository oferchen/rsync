//! Process I/O priority helpers.

use std::io;

/// Linux `ioprio_set` class values from `<linux/ioprio.h>`.
pub const IOPRIO_CLASS_NONE: u8 = 0;
/// Real-time I/O scheduling class.
pub const IOPRIO_CLASS_RT: u8 = 1;
/// Best-effort I/O scheduling class.
pub const IOPRIO_CLASS_BE: u8 = 2;
/// Idle I/O scheduling class.
pub const IOPRIO_CLASS_IDLE: u8 = 3;

/// Sets the calling process's I/O scheduling class and priority level.
///
/// Uses `SYS_ioprio_set` on Linux and returns `io::ErrorKind::Unsupported` on
/// other platforms, matching upstream `niceness.c:apply_io_priority()`.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn set_io_priority(kernel_class: u8, data: u8) -> io::Result<()> {
    const IOPRIO_WHO_PROCESS: libc::c_int = 1;
    const IOPRIO_CLASS_SHIFT: u32 = 13;
    let ioprio = ((kernel_class as libc::c_int) << IOPRIO_CLASS_SHIFT) | (data as libc::c_int);
    // SAFETY: `SYS_ioprio_set` with `IOPRIO_WHO_PROCESS` and pid `0` targets
    // the calling process with scalar integer arguments only.
    let rc = unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, ioprio) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Sets the calling process's I/O scheduling class and priority level.
#[cfg(not(target_os = "linux"))]
pub fn set_io_priority(_kernel_class: u8, _data: u8) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Sets the calling process's CPU scheduling priority (nice value).
///
/// upstream: `niceness.c:279` - `setpriority(PRIO_PROCESS, 0, priority)`;
/// the OS error is returned unchanged. Returns `io::ErrorKind::Unsupported`
/// on targets without `setpriority`.
#[cfg(unix)]
pub fn set_cpu_priority(priority: i32) -> io::Result<()> {
    rustix::process::setpriority_process(None, priority).map_err(io::Error::from)
}

/// Sets the calling process's CPU scheduling priority (nice value).
#[cfg(not(unix))]
pub fn set_cpu_priority(_priority: i32) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-applying the current nice value must succeed and leave it unchanged:
    /// `setpriority` is the only path `--nice` has to the scheduler.
    #[cfg(unix)]
    #[test]
    fn set_cpu_priority_applies_the_requested_nice_value() {
        let current = rustix::process::getpriority_process(None).expect("getpriority");
        set_cpu_priority(current).expect("setpriority to the current value");
        assert_eq!(
            rustix::process::getpriority_process(None).expect("getpriority"),
            current
        );
        let lower = (current + 1).min(19);
        set_cpu_priority(lower).expect("lowering priority is always permitted");
        assert_eq!(
            rustix::process::getpriority_process(None).expect("getpriority"),
            lower
        );
    }

    /// Without `setpriority` the call reports `Unsupported`, which `--nice`
    /// turns into upstream's "not supported on this system" warning.
    #[cfg(not(unix))]
    #[test]
    fn set_cpu_priority_is_unsupported_without_setpriority() {
        let err = set_cpu_priority(0).expect_err("unsupported");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
