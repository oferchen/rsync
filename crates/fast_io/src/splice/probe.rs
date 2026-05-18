//! Kernel-capability detection for `splice(2)`.

#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// Whether `splice` is supported on this kernel. Cached after first probe.
#[cfg(target_os = "linux")]
static SPLICE_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Returns whether `splice(2)` is available on the current system.
///
/// The result is probed once and cached for the lifetime of the process.
/// On non-Linux platforms, always returns `false`.
pub fn is_splice_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        *SPLICE_SUPPORTED.get_or_init(probe_splice_support)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Returns whether `splice(2)` is available and not disabled by `policy`.
///
/// Combines [`is_splice_available`] with the
/// [`ZeroCopyPolicy`](crate::ZeroCopyPolicy) gate. Returns `false` when
/// the policy is [`ZeroCopyPolicy::Disabled`](crate::ZeroCopyPolicy::Disabled)
/// regardless of kernel support, so callers can opt out of zero-copy
/// transfer paths via `--no-zero-copy`.
#[must_use]
pub fn is_splice_enabled(policy: crate::ZeroCopyPolicy) -> bool {
    !matches!(policy, crate::ZeroCopyPolicy::Disabled) && is_splice_available()
}

/// Probes splice support by creating a pipe pair and attempting a zero-length splice.
///
/// This detects kernels or seccomp profiles that block `splice(2)`.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn probe_splice_support() -> bool {
    use super::SplicePipe;

    let pipe = match SplicePipe::new() {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Attempt a zero-length splice from the read end to the write end.
    // We use the pipe's own read end as the "input fd" - this is not useful
    // for real I/O but tests that the syscall is not blocked by seccomp.
    // SAFETY: pipe fds are valid and open. Zero-length splice is a no-op.
    let result = unsafe {
        libc::splice(
            pipe.read_fd(),
            std::ptr::null_mut(),
            pipe.write_fd(),
            std::ptr::null_mut(),
            0,
            0,
        )
    };

    // Result of 0 means the syscall is available (zero bytes transferred).
    // ENOSYS means the syscall does not exist.
    if result < 0 {
        let err = std::io::Error::last_os_error();
        // EAGAIN is acceptable for non-blocking fds with no data - the syscall exists
        err.raw_os_error() == Some(libc::EAGAIN)
    } else {
        true
    }
}
