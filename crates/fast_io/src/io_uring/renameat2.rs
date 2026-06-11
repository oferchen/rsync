//! `IORING_OP_RENAMEAT` (RENAMEAT2) submission helpers and kernel probe.
//!
//! `IORING_OP_RENAMEAT` was added in Linux 5.11 and exposes the same
//! capabilities as the userspace `renameat2(2)` syscall, including the
//! `RENAME_NOREPLACE`, `RENAME_EXCHANGE`, and `RENAME_WHITEOUT` flags. This
//! module wraps `io_uring::opcode::RenameAt` with an availability probe that
//! mirrors the patterns already used for `IORING_OP_POLL_ADD`
//! (see `super::shared_ring`) and the splice syscall
//! (see [`crate::splice::is_splice_available`]).
//!
//! # Probe semantics
//!
//! [`renameat2_supported`] returns `true` only when both:
//!
//! 1. [`is_io_uring_available`] reports the base subsystem usable, and
//! 2. `IORING_REGISTER_PROBE` reports opcode 35 (`IORING_OP_RENAMEAT`)
//!    as supported.
//!
//! The result is cached in a `OnceLock` so subsequent calls are a single
//! atomic load. When the probe registration itself fails (extremely old
//! kernels or seccomp-restricted environments) the probe falls back to
//! `false` because, unlike `POLL_ADD`, RENAMEAT was added much later than
//! the io_uring subsystem itself and cannot be safely assumed.
//!
//! # SQE construction
//!
//! [`build_renameat2_sqe`] returns an [`io_uring::squeue::Entry`] with no
//! `user_data` set; callers tag the entry with their own demux scheme via
//! [`io_uring::squeue::Entry::user_data`]. The CStr borrows must outlive
//! the SQE submission and the kernel completion (the io-uring crate enforces
//! this with the `RenameAt` builder accepting raw `*const c_char`).
//!
//! # Upstream reference
//!
//! Upstream rsync uses `renameat(2)` for atomic temp-file commits in
//! `generator.c:rename_tmp_file()`. The `RENAME_NOREPLACE` flag matches
//! the `--ignore-existing` invariant; `RENAME_EXCHANGE` allows atomic swap
//! of two existing names without deleting either. `RENAME_WHITEOUT` is an
//! overlayfs-only primitive included for completeness because the kernel
//! UAPI exposes all three flags through the same syscall and SQE field.

use std::ffi::CStr;
use std::io;
use std::os::raw::c_int;
use std::sync::OnceLock;

use io_uring::{IoUring as RawIoUring, opcode, squeue, types};

use super::config::is_io_uring_available;

pub use crate::io_uring_common::{
    IORING_OP_RENAMEAT, RENAME_EXCHANGE, RENAME_NOREPLACE, RENAME_WHITEOUT,
};

/// Cached result of the RENAMEAT2 opcode probe.
static RENAMEAT2_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Returns whether `IORING_OP_RENAMEAT` is usable on this kernel.
///
/// The result is probed once and cached for the lifetime of the process.
/// Returns `false` immediately when the base io_uring subsystem is
/// unavailable, when probe registration fails, or when the probe explicitly
/// reports the opcode as unsupported. On non-Linux platforms the stub in
/// `crate::io_uring_stub` always returns `false`.
#[must_use]
pub fn renameat2_supported() -> bool {
    *RENAMEAT2_SUPPORTED.get_or_init(probe_renameat2_support)
}

/// Probes opcode 35 by building a transient ring and calling
/// `IORING_REGISTER_PROBE`.
fn probe_renameat2_support() -> bool {
    if !is_io_uring_available() {
        return false;
    }
    let ring = match RawIoUring::new(2) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_err() {
        // Probe registration itself is not supported. Unlike POLL_ADD which
        // predates the probe API, RENAMEAT was added in 5.11 (probe API in
        // 5.6) so a missing probe means a kernel old enough to lack
        // RENAMEAT entirely.
        return false;
    }
    probe.is_supported(IORING_OP_RENAMEAT)
}

/// Borrowed arguments for an `IORING_OP_RENAMEAT` submission.
///
/// All paths are borrowed `&CStr` so callers retain ownership of the path
/// storage and the borrow checker enforces that the storage outlives the
/// `RenameAt2Args`. The directory file descriptors follow the standard
/// `*at` syscall convention (`AT_FDCWD` for cwd-relative paths).
#[derive(Debug, Clone, Copy)]
pub struct RenameAt2Args<'a> {
    /// Directory fd that `old_path` is resolved against (`AT_FDCWD` for cwd).
    pub old_dir_fd: c_int,
    /// Old path; must outlive the SQE submission and completion.
    pub old_path: &'a CStr,
    /// Directory fd that `new_path` is resolved against (`AT_FDCWD` for cwd).
    pub new_dir_fd: c_int,
    /// New path; must outlive the SQE submission and completion.
    pub new_path: &'a CStr,
    /// Bitwise OR of [`RENAME_NOREPLACE`], [`RENAME_EXCHANGE`], and
    /// [`RENAME_WHITEOUT`]. Zero is a plain rename equivalent to
    /// `renameat(2)`.
    pub flags: u32,
}

/// Builds an `IORING_OP_RENAMEAT` SQE after verifying kernel support.
///
/// Returns [`io::ErrorKind::Unsupported`] when [`renameat2_supported`]
/// reports `false`, so callers can fall back to a synchronous
/// `renameat2(2)` call (or to plain `renameat(2)` for `flags == 0`).
///
/// The returned entry has no `user_data` set; the caller is expected to tag
/// it via [`io_uring::squeue::Entry::user_data`] before pushing it onto a
/// submission queue.
pub fn build_renameat2_sqe(args: RenameAt2Args<'_>) -> io::Result<squeue::Entry> {
    if !renameat2_supported() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_RENAMEAT is not supported on this kernel",
        ));
    }
    Ok(build_renameat2_sqe_unchecked(args))
}

/// Builds an `IORING_OP_RENAMEAT` SQE without consulting the kernel probe.
///
/// Exposed for unit tests that need to exercise the SQE wiring (encoded
/// opcode, flags field, dirfd handling) without depending on whether the
/// host kernel actually supports the opcode. Production callers should use
/// [`build_renameat2_sqe`] so missing kernel support is surfaced as a
/// proper [`io::ErrorKind::Unsupported`] rather than as a deferred CQE
/// `-EINVAL`.
#[must_use]
pub fn build_renameat2_sqe_unchecked(args: RenameAt2Args<'_>) -> squeue::Entry {
    opcode::RenameAt::new(
        types::Fd(args.old_dir_fd),
        args.old_path.as_ptr(),
        types::Fd(args.new_dir_fd),
        args.new_path.as_ptr(),
    )
    .flags(args.flags)
    .build()
}

/// Synchronously submits a RENAMEAT2 SQE on a transient io_uring instance
/// and returns the kernel's CQE result.
///
/// This is a convenience wrapper used by tests and by callers that do not
/// want to manage their own ring lifecycle. The CQE result is returned
/// verbatim (negative `-errno` on failure, `0` on success). On kernels
/// without RENAMEAT support, returns [`io::ErrorKind::Unsupported`].
///
/// The ring is dropped when the call returns, so callers driving
/// many renames should build their own ring and reuse it via
/// [`build_renameat2_sqe`] for amortised submission cost.
pub fn renameat2_blocking(args: RenameAt2Args<'_>) -> io::Result<i32> {
    let entry = build_renameat2_sqe(args)?.user_data(0);
    let mut ring = RawIoUring::new(2)?;
    // SAFETY: `entry` borrows the CStrs in `args`, which outlive this call
    // because they outlive the function (caller holds the borrow). The
    // SQE is consumed in-kernel by submit_and_wait(1) before we return.
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::other("submission queue full"))?;
    }
    ring.submit_and_wait(1)?;
    let cqe = ring
        .completion()
        .next()
        .ok_or_else(|| io::Error::other("submit_and_wait(1) returned but no CQE was reaped"))?;
    Ok(cqe.result())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn opcode_constant_matches_kernel_uapi() {
        // Sanity check against the kernel UAPI value. If the io-uring crate
        // ever exposes a `CODE` associated constant for `opcode::RenameAt`,
        // this assert keeps that path honest as well.
        assert_eq!(IORING_OP_RENAMEAT, 35);
    }

    #[test]
    fn libc_flags_are_re_exported() {
        // Re-export verification: the constants must be addressable via
        // this module so callers do not have to depend on libc directly.
        assert_eq!(RENAME_NOREPLACE, libc::RENAME_NOREPLACE);
        assert_eq!(RENAME_EXCHANGE, libc::RENAME_EXCHANGE);
        assert_eq!(RENAME_WHITEOUT, libc::RENAME_WHITEOUT);
        // The kernel UAPI assigns these distinct bit positions.
        assert_eq!(RENAME_NOREPLACE, 1);
        assert_eq!(RENAME_EXCHANGE, 2);
        assert_eq!(RENAME_WHITEOUT, 4);
    }

    #[test]
    fn probe_is_idempotent() {
        // Caching contract: the probe value must not change between calls.
        let first = renameat2_supported();
        let second = renameat2_supported();
        assert_eq!(first, second);
    }

    #[test]
    fn probe_implies_io_uring_available() {
        // Logical implication: RENAMEAT support is impossible without the
        // base subsystem.
        if renameat2_supported() {
            assert!(is_io_uring_available());
        }
    }

    #[test]
    fn unchecked_sqe_smoke() {
        // Ensure SQE construction does not panic and accepts every flag
        // combination the kernel UAPI exposes. We do not submit the SQE;
        // this exercises only the io-uring crate's builder.
        let old = CString::new("/tmp/oc-rsync-renameat2-old").unwrap();
        let new = CString::new("/tmp/oc-rsync-renameat2-new").unwrap();
        for flags in [
            0,
            RENAME_NOREPLACE,
            RENAME_EXCHANGE,
            RENAME_WHITEOUT,
            RENAME_NOREPLACE | RENAME_WHITEOUT,
        ] {
            let args = RenameAt2Args {
                old_dir_fd: libc::AT_FDCWD,
                old_path: &old,
                new_dir_fd: libc::AT_FDCWD,
                new_path: &new,
                flags,
            };
            let _entry = build_renameat2_sqe_unchecked(args);
        }
    }

    #[test]
    fn checked_sqe_matches_probe() {
        // The checked builder must agree with the cached probe: it returns
        // `Ok` exactly when the probe is true, and `Unsupported` otherwise.
        let old = CString::new("/tmp/oc-rsync-renameat2-old").unwrap();
        let new = CString::new("/tmp/oc-rsync-renameat2-new").unwrap();
        let args = RenameAt2Args {
            old_dir_fd: libc::AT_FDCWD,
            old_path: &old,
            new_dir_fd: libc::AT_FDCWD,
            new_path: &new,
            flags: 0,
        };
        match build_renameat2_sqe(args) {
            Ok(_) => assert!(renameat2_supported()),
            Err(e) => {
                assert!(!renameat2_supported());
                assert_eq!(e.kind(), io::ErrorKind::Unsupported);
            }
        }
    }
}
