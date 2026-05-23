//! SQM-3: wire mmap'd basis windows in memory before SQPOLL submissions.
//!
//! Pairs an `IORING_SETUP_SQPOLL` ring with a file-backed mmap by pinning the
//! basis-window pages via `mlock(2)` for the duration of the submission cycle.
//! With the wired range backed by present PTEs, the SQPOLL kthread cannot
//! take a fault on a page the userspace task has not yet touched - which is
//! the race surface documented in
//! `docs/audits/io_uring_sqpoll_mmap_pagefault.md` and reproduced by
//! `crates/fast_io/tests/repro_sqpoll_mmap.rs`.
//!
//! Implements Candidate 2 from `docs/design/sqm-1c-workaround-spec.md`,
//! locked by `docs/design/sqm-2b-implementation-design.md`. Candidate 3
//! (the defensive SQPOLL disable already living in
//! `crate::io_uring::config::build_ring`) stays as the unconditional
//! fallback when `mlock` returns a downgrade-class errno.
//!
//! ## Window granularity
//!
//! Per-SQE-batch (one wired window per submission cycle), sized at
//! `sq_entries * buffer_size`. The default of `64 * 64 KiB = 4 MiB` is the
//! SQM-2.b-blessed value; the pin caps at `MAX_WIRED_WINDOW_BYTES` so a
//! caller passing a multi-GiB basis never tries to wire the whole file.
//!
//! ## Error policy
//!
//! `EAGAIN`, `EPERM`, and `ENOMEM` are downgrade-class: log once per ring
//! lifetime and return [`MlockError::Downgrade`]. Callers route the
//! submission through the regular (non-SQPOLL) ring. Any other errno is
//! [`MlockError::Fatal`] and surfaces to the transfer.
//!
//! ## Counters
//!
//! Two process-wide atomic counters track the wrapper's behaviour:
//!
//! - [`mlock_attempts`] - incremented on every successful pin.
//! - [`mlock_downgrades`] - incremented on every downgrade-class errno.
//!
//! The ratio `mlock_downgrades / mlock_attempts >= 0.05` is the SQM-2.b
//! rollback trigger.

use std::io;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum bytes pinned per `WiredBasisWindow::new` call.
///
/// Sized at `sq_entries * buffer_size` for the default ring shape
/// (64 SQEs * 64 KiB buffers = 4 MiB). Per SQM-2.b section "Wiring
/// granularity": per-file wiring would exceed `RLIMIT_MEMLOCK` immediately
/// on real multi-GiB basis files, while individual per-SQE wiring would
/// issue `mlock`/`munlock` thousands of times per second. Per-batch at this
/// cap is the design-blessed middle.
pub const MAX_WIRED_WINDOW_BYTES: usize = 4 * 1024 * 1024;

/// Process-wide count of [`WiredBasisWindow::new`] entries that wired
/// successfully.
///
/// Monotonic, never resets. Paired with [`MLOCK_DOWNGRADES`] to compute the
/// SQM-2.b rollback ratio.
static MLOCK_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of [`WiredBasisWindow::new`] calls that returned
/// [`MlockError::Downgrade`].
///
/// Monotonic, never resets. The ratio
/// `MLOCK_DOWNGRADES / MLOCK_ATTEMPTS >= 0.05` is the documented
/// revert trigger; operators flip the `sqpoll-mlock-basis` cargo feature
/// off when the threshold trips for two consecutive CI cycles.
static MLOCK_DOWNGRADES: AtomicU64 = AtomicU64::new(0);

/// One-shot guard so the `EPERM` / `EAGAIN` / `ENOMEM` warning only fires
/// once per process. Subsequent downgrades still bump the counter but stay
/// silent in the log to avoid flooding. Linux-only since the warning path
/// lives inside the Linux-gated [`WiredBasisWindow::new`].
#[cfg(target_os = "linux")]
static DOWNGRADE_WARNED: AtomicBool = AtomicBool::new(false);

/// Returns the cumulative count of successful basis-window pins.
#[must_use]
pub fn mlock_attempts() -> u64 {
    MLOCK_ATTEMPTS.load(Ordering::Relaxed)
}

/// Returns the cumulative count of downgrade-class `mlock` failures.
///
/// A non-zero value indicates [`WiredBasisWindow::new`] hit `EAGAIN`,
/// `EPERM`, or `ENOMEM` at least once and the caller fell back to the
/// regular (non-SQPOLL) ring. Compare against [`mlock_attempts`] to
/// produce the rollback ratio.
#[must_use]
pub fn mlock_downgrades() -> u64 {
    MLOCK_DOWNGRADES.load(Ordering::Relaxed)
}

/// Outcome of an `mlock` attempt classified into the SQM-2.b error policy.
#[derive(Debug)]
pub enum MlockError {
    /// `EAGAIN` (rlimit hit), `EPERM` (lacks `CAP_IPC_LOCK`), or `ENOMEM`
    /// (kernel cannot pin). Caller should route the submission through the
    /// regular ring.
    Downgrade(io::Error),
    /// `EINVAL` or any other unexpected errno. Treat as a programmer bug;
    /// surface to the transfer.
    Fatal(io::Error),
}

impl MlockError {
    /// Returns the underlying [`io::Error`] regardless of classification.
    #[must_use]
    pub fn into_io(self) -> io::Error {
        match self {
            Self::Downgrade(e) | Self::Fatal(e) => e,
        }
    }
}

impl std::fmt::Display for MlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Downgrade(e) => write!(f, "mlock downgrade: {e}"),
            Self::Fatal(e) => write!(f, "mlock fatal: {e}"),
        }
    }
}

impl std::error::Error for MlockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Downgrade(e) | Self::Fatal(e) => Some(e),
        }
    }
}

/// Returns `true` when the errno belongs to the downgrade-class set
/// (`EAGAIN`, `EPERM`, `ENOMEM`).
#[cfg(target_os = "linux")]
fn is_downgrade_errno(raw: i32) -> bool {
    matches!(raw, libc::EAGAIN | libc::EPERM | libc::ENOMEM)
}

/// RAII guard wrapping a `mlock`/`munlock` pair around a basis-window
/// address range.
///
/// The guard owns no allocation; it borrows the address/length pair from
/// the caller's existing `MmapReader` slice and pins those pages via
/// `mlock(2)` for the guard's lifetime. `Drop` calls `munlock(2)` so the
/// kernel can reclaim the wired pages.
///
/// # Drop ordering
///
/// The guard MUST outlive the SQPOLL submit/reap cycle. The submission
/// completes only after the CQE is drained, which means callers must hold
/// the guard alive across `submit_and_wait`. Dropping early would unpin
/// the pages while the SQPOLL kthread may still be reading them, which
/// re-opens the original race surface this wrapper closes.
///
/// # Safety contract for the kernel call
///
/// `mlock(addr, len)` requires that the byte range `[addr, addr + len)` is
/// a valid mapping in the calling process. Callers MUST source the pointer
/// from an `MmapReader` (or equivalent live mmap) that outlives this
/// guard; the wrapper does not own the mapping and cannot extend its
/// lifetime.
#[cfg(target_os = "linux")]
pub struct WiredBasisWindow {
    addr: *const libc::c_void,
    len: usize,
}

#[cfg(target_os = "linux")]
impl WiredBasisWindow {
    /// Pins the byte range `[addr, addr + len.min(MAX_WIRED_WINDOW_BYTES))`
    /// via `mlock(2)`.
    ///
    /// On success the returned guard increments [`mlock_attempts`] and
    /// the wired pages stay resident until `Drop`. On `EAGAIN` / `EPERM` /
    /// `ENOMEM` the guard increments [`mlock_downgrades`] and returns
    /// [`MlockError::Downgrade`]; the caller falls back to a non-SQPOLL
    /// ring. Any other errno produces [`MlockError::Fatal`].
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `[addr, addr + len)` falls inside a
    /// live mmap mapping owned by the current process for the entire
    /// lifetime of the returned guard. Violating this invariant is
    /// undefined behaviour from the kernel's perspective; an `EFAULT`
    /// would also be valid grounds for a `Fatal` return.
    pub fn new(addr: *const u8, len: usize) -> Result<Self, MlockError> {
        let len = len.min(MAX_WIRED_WINDOW_BYTES);
        if len == 0 {
            MLOCK_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            return Ok(Self {
                addr: std::ptr::null(),
                len: 0,
            });
        }
        let ptr = addr.cast::<libc::c_void>();
        // SAFETY: the caller's contract on `WiredBasisWindow::new` requires
        // `[addr, addr + len)` to lie inside a live mmap of the current
        // process. `mlock` reads no userspace memory; it only walks the page
        // tables for the given range. A misaligned address produces `EINVAL`
        // and is reported as `Fatal`, not undefined behaviour.
        let rc = unsafe { libc::mlock(ptr, len) };
        if rc == 0 {
            MLOCK_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            Ok(Self { addr: ptr, len })
        } else {
            let err = io::Error::last_os_error();
            let raw = err.raw_os_error().unwrap_or(0);
            if is_downgrade_errno(raw) {
                MLOCK_DOWNGRADES.fetch_add(1, Ordering::Relaxed);
                if !DOWNGRADE_WARNED.swap(true, Ordering::Relaxed) {
                    logging::debug_log!(
                        Io,
                        1,
                        "mlock downgrade: errno={raw} ({err}); SQPOLL basis-window pin failed, \
                         falling back to regular ring (SQM-3)"
                    );
                }
                Err(MlockError::Downgrade(err))
            } else {
                Err(MlockError::Fatal(err))
            }
        }
    }

    /// Returns the wired-window length in bytes (post-clamp).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the wired window has zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the wired window's starting address.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.addr.cast::<u8>()
    }
}

#[cfg(target_os = "linux")]
impl Drop for WiredBasisWindow {
    fn drop(&mut self) {
        if self.len == 0 || self.addr.is_null() {
            return;
        }
        // SAFETY: `mlock` succeeded on this exact `(addr, len)` pair in
        // `new`, so the kernel mapping is still wired and present. `munlock`
        // walks the same page-table range to clear `VM_LOCKED`; it touches
        // no userspace memory.
        let rc = unsafe { libc::munlock(self.addr, self.len) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            logging::debug_log!(
                Io,
                1,
                "munlock failed during WiredBasisWindow drop: {err}; pages stay wired until \
                 process exit"
            );
        }
    }
}

/// Stub guard for non-Linux targets. SQPOLL does not exist outside Linux,
/// so this construction always succeeds with a zero-cost no-op.
#[cfg(not(target_os = "linux"))]
pub struct WiredBasisWindow {
    len: usize,
}

#[cfg(not(target_os = "linux"))]
impl WiredBasisWindow {
    /// No-op stub for non-Linux platforms. SQPOLL is Linux-only, so the
    /// wrapper has nothing to wire. Always returns `Ok` so cross-platform
    /// callers can share the same API.
    #[allow(clippy::missing_errors_doc)]
    pub fn new(_addr: *const u8, len: usize) -> Result<Self, MlockError> {
        Ok(Self {
            len: len.min(MAX_WIRED_WINDOW_BYTES),
        })
    }

    /// Returns the wired-window length in bytes (post-clamp).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the wired window has zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns a null pointer; the stub has nothing wired.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        std::ptr::null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn downgrade_error_into_io_preserves_errno() {
        let raw = io::Error::from_raw_os_error(libc::EAGAIN);
        let err = MlockError::Downgrade(raw);
        let recovered = err.into_io();
        assert_eq!(recovered.raw_os_error(), Some(libc::EAGAIN));
    }

    #[cfg(unix)]
    #[test]
    fn fatal_error_into_io_preserves_errno() {
        let raw = io::Error::from_raw_os_error(libc::EINVAL);
        let err = MlockError::Fatal(raw);
        let recovered = err.into_io();
        assert_eq!(recovered.raw_os_error(), Some(libc::EINVAL));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn is_downgrade_errno_classifies_correct_set() {
        assert!(is_downgrade_errno(libc::EAGAIN));
        assert!(is_downgrade_errno(libc::EPERM));
        assert!(is_downgrade_errno(libc::ENOMEM));
        assert!(!is_downgrade_errno(libc::EINVAL));
        assert!(!is_downgrade_errno(libc::EFAULT));
        assert!(!is_downgrade_errno(0));
    }

    #[test]
    fn zero_length_window_is_no_op() {
        let before = mlock_attempts();
        let dummy: [u8; 0] = [];
        let window = WiredBasisWindow::new(dummy.as_ptr(), 0).expect("zero-length succeeds");
        assert_eq!(window.len(), 0);
        assert!(window.is_empty());
        // Zero-length entry still bumps the attempt counter for parity with
        // the live path so the rollback ratio remains well-defined. Use
        // strict `>` so the assertion stays robust against concurrent
        // tests that share the same process-wide counter.
        assert!(mlock_attempts() > before);
        drop(window);
    }

    #[test]
    fn max_window_size_matches_design() {
        // SQM-2.b "Wiring granularity": sq_entries * buffer_size with the
        // 64 * 64 KiB default ring shape.
        assert_eq!(MAX_WIRED_WINDOW_BYTES, 64 * 64 * 1024);
        assert_eq!(MAX_WIRED_WINDOW_BYTES, 4 * 1024 * 1024);
    }
}
