//! Stub config / availability detection mirroring [`crate::io_uring::config`].
//!
//! Every entry point reports the backend as unavailable on this platform.

use crate::io_uring_common::{IoBackend, IoUringKernelInfo};

/// Marker type implementing [`IoBackend`] for the no-op stub backend.
///
/// Used by code that needs to query availability through the cross-platform
/// trait without caring which backend was compiled. Always reports the
/// backend as unavailable on this platform.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubIoUringBackend;

impl IoBackend for StubIoUringBackend {
    fn is_available() -> bool {
        false
    }

    fn availability_reason() -> String {
        "io_uring: disabled (not built for this target)".to_string()
    }
}

/// Check whether io_uring is available (always `false` on this platform).
#[must_use]
pub fn is_io_uring_available() -> bool {
    false
}

/// Returns whether SQPOLL was requested but fell back (always `false` on this platform).
#[must_use]
pub fn sqpoll_fell_back() -> bool {
    false
}

/// Records the process-wide SQPOLL opt-out (no-op on this platform).
///
/// Linux exposes a real atomic gate via
/// [`crate::io_uring::set_sqpoll_disabled_by_policy`]. On non-Linux
/// targets there is no SQPOLL kthread to suppress, so the stub keeps the
/// symbol available for cross-platform CLI wiring but does nothing.
pub fn set_sqpoll_disabled_by_policy() {}

/// Returns whether the SQPOLL opt-out has been set (always `false` on this platform).
///
/// Mirrors the Linux query so cross-platform callers can use the same
/// import path. The stub always reports `false` because there is no
/// SQPOLL kthread to begin with.
#[must_use]
pub fn is_sqpoll_disabled_by_policy() -> bool {
    false
}

/// Public accessors for kernel version detection used by `--version` output.
///
/// Mirrors the real Linux module so cross-platform callers can use the same
/// import path; every function reports unavailability on this platform.
pub mod config_detail {
    use super::{IoUringKernelInfo, StubIoUringBackend};
    use crate::io_uring_common::IoBackend;

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn parse_kernel_version(_release: &str) -> Option<(u32, u32)> {
        None
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn get_kernel_release_string() -> Option<String> {
        None
    }

    /// Returns a human-readable reason for io_uring unavailability.
    #[must_use]
    pub fn io_uring_availability_reason() -> String {
        StubIoUringBackend::availability_reason()
    }

    /// Returns a stub [`IoUringKernelInfo`] populated for unavailability.
    #[must_use]
    pub fn io_uring_kernel_info() -> IoUringKernelInfo {
        IoUringKernelInfo {
            available: false,
            kernel_major: None,
            kernel_minor: None,
            supported_ops: 0,
            pbuf_ring_supported: false,
            reason: io_uring_availability_reason(),
        }
    }
}
