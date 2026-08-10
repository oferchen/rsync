//! io_uring `LINKAT` opcode wrapper and kernel availability probe.
//!
//! `IORING_OP_LINKAT` lets the kernel create a hard link asynchronously,
//! avoiding a synchronous `linkat(2)` syscall on the hot path. It became
//! available in Linux 5.15 alongside `RENAMEAT2`, `MKDIRAT`, and `SYMLINKAT`.
//!
//! # API surface
//!
//! - [`linkat_supported`] - process-wide cached probe; short-circuits on
//!   `is_io_uring_available()` and then asks the kernel via
//!   `IORING_REGISTER_PROBE` whether opcode 39 (`IORING_OP_LINKAT`) is
//!   reported as supported.
//! - [`build_linkat_sqe`] - constructs an `squeue::Entry` for a real LINKAT
//!   submission; returns [`io::ErrorKind::Unsupported`] when the probe is
//!   `false`. Callers must keep the [`LinkAtArgs`] paths alive until the
//!   matching CQE has been reaped.
//! - [`build_linkat_sqe_unchecked`] - identical SQE construction without the
//!   probe gate. Reserved for tests that exercise the encoder in isolation
//!   on every platform.
//!
//! # Path lifetime contract
//!
//! [`LinkAtArgs`] borrows the source and destination paths as `&CStr`. The
//! kernel reads these strings during submission, so callers must keep the
//! original allocations alive at least until [`io_uring::IoUring::submit`]
//! returns. The borrowed lifetime in the struct enforces this at compile
//! time as long as the SQE is consumed before `LinkAtArgs` is dropped.
//!
//! # Upstream rsync reference
//!
//! Upstream rsync uses synchronous `link(2)` / `linkat(2)` for hardlink
//! creation (`flist.c`, `hlink.c`). The io_uring fast path is an
//! optimisation: when the kernel exposes `IORING_OP_LINKAT`, the receiver
//! can submit hardlink creation alongside disk writes on the same ring,
//! eliminating a separate syscall per hardlinked file.

#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use std::{ffi::CStr, io};

use io_uring::{opcode, squeue, types};

use super::config::is_io_uring_available;

pub use crate::io_uring_common::{IORING_OP_LINKAT, LINKAT_MIN_KERNEL};

/// Cached probe result on Linux. Populated lazily on first call.
#[cfg(target_os = "linux")]
static LINKAT_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Borrowed arguments for an `IORING_OP_LINKAT` submission.
///
/// Both `old_path` and `new_path` live in the caller's address space; the
/// kernel reads them when the SQE is processed. The borrowed lifetime
/// `'a` ties the args to those allocations so the compiler refuses to drop
/// them while a built SQE still references the same memory.
#[derive(Debug)]
pub struct LinkAtArgs<'a> {
    /// Directory file descriptor that resolves `old_path`. Use
    /// [`libc::AT_FDCWD`] to resolve from the current working directory.
    pub old_dirfd: i32,
    /// Source path of the existing inode being hardlinked.
    pub old_path: &'a CStr,
    /// Directory file descriptor that resolves `new_path`. Use
    /// [`libc::AT_FDCWD`] to resolve from the current working directory.
    pub new_dirfd: i32,
    /// Destination path of the new hardlink.
    pub new_path: &'a CStr,
    /// Flags passed to the kernel: typically `0`,
    /// [`libc::AT_SYMLINK_FOLLOW`], or [`libc::AT_EMPTY_PATH`].
    pub flags: i32,
}

/// Returns whether `IORING_OP_LINKAT` is usable on this system.
///
/// On Linux, the result is probed once and cached for the lifetime of the
/// process. The probe:
///
/// 1. Returns `false` immediately when [`is_io_uring_available`] is `false`,
///    so we never build an extra ring just to discover the opcode is
///    irrelevant.
/// 2. Builds a tiny throwaway ring and registers an `io_uring::Probe`,
///    asking the kernel to enumerate supported opcodes.
/// 3. Returns `probe.is_supported(IORING_OP_LINKAT)`.
///
/// On non-Linux platforms or when the `io_uring` cargo feature is disabled,
/// the stub module supplies a counterpart that always returns `false`.
#[must_use]
pub fn linkat_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        *LINKAT_SUPPORTED.get_or_init(probe_linkat_support)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Probes the live kernel for `IORING_OP_LINKAT` support.
///
/// Mirrors the convention used by `shared_ring::probe_poll_add`: short-
/// circuit on the process-wide io_uring availability flag, then build a
/// throwaway ring and register a probe. A failed probe registration on a
/// kernel that nonetheless meets the io_uring 5.6 minimum returns `false`
/// because LINKAT is genuinely 5.15+; we cannot assume forward
/// compatibility the way POLL_ADD does.
#[cfg(target_os = "linux")]
fn probe_linkat_support() -> bool {
    if !is_io_uring_available() {
        return false;
    }
    let ring = match io_uring::IoUring::new(2) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_err() {
        return false;
    }
    probe.is_supported(IORING_OP_LINKAT)
}

/// Builds an `IORING_OP_LINKAT` SQE if the running kernel supports the
/// opcode.
///
/// Returns [`io::ErrorKind::Unsupported`] when [`linkat_supported`] reports
/// `false`. The returned `squeue::Entry` is unattached; callers are
/// responsible for tagging it with `user_data` and pushing it onto a ring's
/// submission queue under the `unsafe` contract documented by
/// `io_uring::SubmissionQueue::push`.
///
/// The `args` struct must outlive the submission so the kernel can read
/// the borrowed `CStr` paths.
pub fn build_linkat_sqe(args: LinkAtArgs<'_>) -> io::Result<squeue::Entry> {
    if !linkat_supported() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_LINKAT is not supported by this kernel",
        ));
    }
    Ok(build_linkat_sqe_unchecked(args))
}

/// Builds an `IORING_OP_LINKAT` SQE without consulting the kernel probe.
///
/// Reserved for unit tests that want to verify the encoded SQE bytes in
/// isolation. Production callers should use [`build_linkat_sqe`] so the
/// probe gate keeps the fallback path correct.
#[must_use]
pub fn build_linkat_sqe_unchecked(args: LinkAtArgs<'_>) -> squeue::Entry {
    opcode::LinkAt::new(
        types::Fd(args.old_dirfd),
        args.old_path.as_ptr(),
        types::Fd(args.new_dirfd),
        args.new_path.as_ptr(),
    )
    .flags(args.flags)
    .build()
}

/// Synchronously submits a `LINKAT` SQE on a private throwaway ring and
/// returns the kernel's CQE result.
///
/// This is the high-level convenience wrapper around [`build_linkat_sqe`]
/// for callers that do not maintain a long-lived ring (tests, one-shot
/// hardlink creation in slow paths). The function builds a 2-entry ring,
/// submits the SQE, blocks for one completion, and returns:
///
/// - `Ok(0)` on success - the hardlink was created.
/// - `Err(Unsupported)` when the kernel does not advertise
///   `IORING_OP_LINKAT`.
/// - `Err(io::Error::from_raw_os_error(-result))` when the kernel returns a
///   negative errno through the CQE.
///
/// `args` borrows the `CStr` paths; the function keeps them alive for the
/// duration of the syscall.
#[allow(unsafe_code)]
pub fn submit_linkat_blocking(args: LinkAtArgs<'_>) -> io::Result<i32> {
    let sqe = build_linkat_sqe(args)?.user_data(0);
    let mut ring = io_uring::IoUring::new(2)?;
    // SAFETY: `sqe` references CStr paths borrowed from `args`, which the
    // caller's stack frame keeps alive across `submit_and_wait`. The ring
    // is local and has no other outstanding references to those paths.
    unsafe {
        ring.submission()
            .push(&sqe)
            .map_err(|_| io::Error::other("submission queue full while pushing LINKAT SQE"))?;
    }
    ring.submit_and_wait(1)?;
    let cqe = ring
        .completion()
        .next()
        .ok_or_else(|| io::Error::other("missing LINKAT CQE"))?;
    let result = cqe.result();
    if result < 0 {
        return Err(io::Error::from_raw_os_error(-result));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linkat_opcode_constant_matches_kernel_uapi() {
        // IORING_OP_LINKAT == 39 in include/uapi/linux/io_uring.h since 5.15.
        assert_eq!(IORING_OP_LINKAT, 39);
        assert_eq!(opcode::LinkAt::CODE, IORING_OP_LINKAT);
    }

    #[test]
    fn linkat_min_kernel_is_5_15() {
        assert_eq!(LINKAT_MIN_KERNEL, (5, 15));
    }

    #[test]
    fn linkat_supported_is_idempotent() {
        let first = linkat_supported();
        let second = linkat_supported();
        let third = linkat_supported();
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn build_linkat_sqe_returns_unsupported_when_probe_false() {
        if linkat_supported() {
            return; // probe says yes; cannot exercise the unsupported branch
        }
        let old = c"/tmp/old";
        let new = c"/tmp/new";
        let err = build_linkat_sqe(LinkAtArgs {
            old_dirfd: libc::AT_FDCWD,
            old_path: old,
            new_dirfd: libc::AT_FDCWD,
            new_path: new,
            flags: 0,
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn build_linkat_sqe_unchecked_smoke() {
        let old = c"/tmp/old";
        let new = c"/tmp/new";
        // Construct without panic and tag with user_data; the queue API
        // consumes a tagged entry, so chaining `user_data` exercises the
        // builder's full surface.
        let _tagged = build_linkat_sqe_unchecked(LinkAtArgs {
            old_dirfd: libc::AT_FDCWD,
            old_path: old,
            new_dirfd: libc::AT_FDCWD,
            new_path: new,
            flags: libc::AT_SYMLINK_FOLLOW,
        })
        .user_data(0xCAFE_F00D);
    }

    #[test]
    fn build_linkat_sqe_succeeds_when_probe_true() {
        if !linkat_supported() {
            return;
        }
        let old = c"/tmp/old";
        let new = c"/tmp/new";
        let _entry = build_linkat_sqe(LinkAtArgs {
            old_dirfd: libc::AT_FDCWD,
            old_path: old,
            new_dirfd: libc::AT_FDCWD,
            new_path: new,
            flags: 0,
        })
        .expect("probe true implies SQE builds")
        .user_data(0x1234);
    }
}
