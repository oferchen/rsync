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
