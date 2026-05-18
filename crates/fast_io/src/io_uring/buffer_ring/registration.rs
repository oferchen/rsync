//! Kernel-side registration plumbing for the provided buffer ring.
//!
//! Owns the `IORING_REGISTER_PBUF_RING` / `IORING_UNREGISTER_PBUF_RING`
//! opcodes, the mmap offset, the kernel version probe (cached process-wide
//! in a [`OnceLock`]) and the `IoUringBufReg` ABI struct used to register
//! and unregister a ring with the kernel.

use std::sync::OnceLock;

use crate::io_uring_common::BufferRingError;

use super::super::config;

/// `IORING_REGISTER_PBUF_RING` opcode (kernel 5.19+).
pub(super) const IORING_REGISTER_PBUF_RING: libc::c_uint = 22;

/// `IORING_UNREGISTER_PBUF_RING` opcode (kernel 5.19+).
pub(super) const IORING_UNREGISTER_PBUF_RING: libc::c_uint = 23;

/// Offset passed to `mmap` to map the provided buffer ring region.
pub(super) const IORING_OFF_PBUF_RING: u64 = 0x80000000;

/// Minimum kernel version for PBUF_RING support.
pub(super) const MIN_PBUF_RING_KERNEL: (u32, u32) = (5, 19);

/// Matches `struct io_uring_buf_reg` from the kernel.
#[repr(C)]
pub(super) struct IoUringBufReg {
    pub ring_addr: u64,
    pub ring_entries: u32,
    pub bgid: u16,
    pub flags: u16,
    pub resv: [u64; 3],
}

/// Process-wide cache for the PBUF_RING support probe.
///
/// Populated by the first call to [`is_supported`] / [`pbuf_ring_supported`]
/// and reused for the lifetime of the process. Caching avoids repeating
/// the `uname(2)` syscall and version parse on every speculative call site.
static PBUF_RING_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Returns `true` if the kernel supports PBUF_RING (>= 5.19).
///
/// Checks the kernel version via `uname(2)`. This is a necessary but not
/// sufficient condition - the actual `IORING_REGISTER_PBUF_RING` call may
/// still fail if seccomp blocks it.
///
/// The result is cached in a process-wide [`OnceLock`], so repeated calls
/// after the first are a single relaxed atomic load. Use
/// [`super::BufferRing::try_new`] when you also need to verify that
/// registration will actually succeed.
#[must_use]
pub fn is_supported() -> bool {
    *PBUF_RING_SUPPORTED.get_or_init(probe_pbuf_ring_support)
}

/// Alias for [`is_supported`] matching the cross-crate naming used by
/// [`crate::pbuf_ring_supported`].
///
/// Provided so callers that import this module directly can use the
/// crate-wide name without going through the top-level re-export.
#[must_use]
pub fn pbuf_ring_supported() -> bool {
    is_supported()
}

/// Performs the actual kernel-version check used by [`is_supported`].
fn probe_pbuf_ring_support() -> bool {
    let release = match config::config_detail::get_kernel_release_string() {
        Some(r) => r,
        None => return false,
    };
    let (major, minor) = match config::parse_kernel_version(&release) {
        Some(v) => v,
        None => return false,
    };
    (major, minor) >= MIN_PBUF_RING_KERNEL
}

/// Checks that the running kernel supports PBUF_RING.
pub(super) fn check_kernel_version() -> Result<(), BufferRingError> {
    let release = config::config_detail::get_kernel_release_string()
        .ok_or(BufferRingError::KernelVersionUnknown)?;
    let (major, minor) =
        config::parse_kernel_version(&release).ok_or(BufferRingError::KernelVersionUnknown)?;
    if (major, minor) < MIN_PBUF_RING_KERNEL {
        return Err(BufferRingError::KernelTooOld { major, minor });
    }
    Ok(())
}
