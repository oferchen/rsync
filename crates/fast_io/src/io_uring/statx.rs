//! io_uring `IORING_OP_STATX` opcode wrapper and batch submission.
//!
//! `IORING_OP_STATX` lets the kernel perform `statx(2)` asynchronously via
//! the io_uring submission queue, avoiding a synchronous syscall per file when
//! stat-ing many files during directory traversal. It became available in
//! Linux 5.11.
//!
//! # API surface
//!
//! - [`statx_supported`] - process-wide cached probe; short-circuits on
//!   `is_io_uring_available()` and then asks the kernel via
//!   `IORING_REGISTER_PROBE` whether opcode 21 (`IORING_OP_STATX`) is
//!   reported as supported.
//! - [`build_statx_sqe`] - constructs an `squeue::Entry` for a STATX
//!   submission; returns [`io::ErrorKind::Unsupported`] when the probe is
//!   `false`. Callers must keep the [`StatxArgs`] path and buffer alive until
//!   the matching CQE has been reaped.
//! - [`build_statx_sqe_unchecked`] - identical SQE construction without the
//!   probe gate. Reserved for tests that exercise the encoder in isolation
//!   on every platform.
//! - [`submit_statx_blocking`] - synchronously submits a single STATX SQE on
//!   a throwaway ring and returns the populated [`rustix::fs::Statx`] result.
//! - [`submit_statx_batch`] - submits multiple STATX SQEs as independent
//!   operations on a single ring and returns all results. Falls back to
//!   synchronous `statx(2)` via `rustix` when the kernel lacks the opcode.
//!
//! # Why [`rustix::fs::Statx`] instead of `libc::statx`
//!
//! The `libc` crate only exposes `statx` (and `STATX_*` constants) on glibc
//! targets. On `linux-musl` builds the symbols are absent, which broke
//! cross-compiles. `rustix` ships its own `#[repr(C)]` mirror of the kernel
//! ABI on every Linux target, so we use it as the public type throughout
//! this module to keep musl, glibc, and other libcs in lockstep.
//!
//! # Path lifetime contract
//!
//! [`StatxArgs`] borrows the pathname as `&CStr`. The kernel reads this
//! string during submission, so callers must keep the original allocation
//! alive at least until the corresponding CQE has been reaped. The borrowed
//! lifetime in the struct enforces this at compile time as long as the SQE
//! is consumed before `StatxArgs` is dropped.
//!
//! # Kernel version requirement
//!
//! `IORING_OP_STATX` requires Linux 5.11+. The runtime probe still asks
//! the kernel directly via `IORING_REGISTER_PROBE` because kernels may
//! backport or disable individual opcodes independently of the reported
//! `uname` release.
//!
//! # Upstream rsync reference
//!
//! Upstream rsync uses synchronous `stat(2)` / `lstat(2)` (and `statx(2)` on
//! newer glibc) for file metadata retrieval during file list building
//! (`flist.c:receive_file_entry()`, `generator.c`). The io_uring fast path
//! is an optimisation: when the kernel exposes `IORING_OP_STATX`, the
//! generator can submit batched stat calls alongside other ring operations,
//! reducing per-file syscall overhead during directory traversal.

#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use std::{ffi::CStr, io, path::Path};

use io_uring::{opcode, squeue, types};

use super::config::is_io_uring_available;

pub use crate::io_uring_common::{IORING_OP_STATX, STATX_MIN_KERNEL};

/// Cached probe result on Linux. Populated lazily on first call.
#[cfg(target_os = "linux")]
static STATX_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Borrowed arguments for an `IORING_OP_STATX` submission.
///
/// The `pathname` lives in the caller's address space; the kernel reads
/// this string when the SQE is processed. The `statx_buf` is written by
/// the kernel on completion. The borrowed lifetime `'a` ties the args
/// to those allocations so the compiler refuses to drop them while a built
/// SQE still references the same memory.
#[derive(Debug)]
pub struct StatxArgs<'a> {
    /// Directory file descriptor that resolves `pathname`. Use
    /// [`libc::AT_FDCWD`] to resolve from the current working directory.
    pub dirfd: i32,
    /// Path to stat. Must outlive the SQE submission and completion.
    pub pathname: &'a CStr,
    /// Flags passed to the kernel: combination of `libc::AT_*` flags.
    /// Common values:
    /// - `0` for following symlinks (like `stat`)
    /// - `libc::AT_SYMLINK_NOFOLLOW` for not following symlinks (like `lstat`)
    /// - `libc::AT_EMPTY_PATH` when using `dirfd` as the target itself
    pub flags: i32,
    /// Mask of fields to request. Use [`rustix::fs::StatxFlags::BASIC_STATS`]
    /// (via [`rustix::fs::StatxFlags::bits`]) for the common fields (mode,
    /// nlink, uid, gid, ino, size, blocks, timestamps).
    pub mask: u32,
    /// Output buffer for the kernel to write the statx result into.
    /// Must be zero-initialized before submission.
    pub statx_buf: &'a mut rustix::fs::Statx,
}

/// Returns whether `IORING_OP_STATX` is usable on this system.
///
/// On Linux, the result is probed once and cached for the lifetime of the
/// process. The probe:
///
/// 1. Returns `false` immediately when [`is_io_uring_available`] is `false`,
///    so we never build an extra ring just to discover the opcode is
///    irrelevant.
/// 2. Builds a tiny throwaway ring and registers an `io_uring::Probe`,
///    asking the kernel to enumerate supported opcodes.
/// 3. Returns `probe.is_supported(IORING_OP_STATX)`.
///
/// On non-Linux platforms or when the `io_uring` cargo feature is disabled,
/// the stub module supplies a counterpart that always returns `false`.
#[must_use]
pub fn statx_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        *STATX_SUPPORTED.get_or_init(probe_statx_support)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Probes the live kernel for `IORING_OP_STATX` support.
///
/// Mirrors the convention used by `shared_ring::probe_poll_add`: short-
/// circuit on the process-wide io_uring availability flag, then build a
/// throwaway ring and register a probe.
#[cfg(target_os = "linux")]
fn probe_statx_support() -> bool {
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
    probe.is_supported(IORING_OP_STATX)
}

/// Builds an `IORING_OP_STATX` SQE if the running kernel supports the
/// opcode.
///
/// Returns [`io::ErrorKind::Unsupported`] when [`statx_supported`] reports
/// `false`. The returned `squeue::Entry` is unattached; callers are
/// responsible for tagging it with `user_data` and pushing it onto a ring's
/// submission queue under the `unsafe` contract documented by
/// `io_uring::SubmissionQueue::push`.
///
/// The `args` struct must outlive the submission so the kernel can read
/// the borrowed `CStr` path and write the statx buffer.
pub fn build_statx_sqe(args: &mut StatxArgs<'_>) -> io::Result<squeue::Entry> {
    if !statx_supported() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_STATX is not supported by this kernel",
        ));
    }
    Ok(build_statx_sqe_unchecked(args))
}

/// Builds an `IORING_OP_STATX` SQE without consulting the kernel probe.
///
/// Reserved for unit tests that want to verify the encoded SQE bytes in
/// isolation. Production callers should use [`build_statx_sqe`] so the
/// probe gate keeps the fallback path correct.
#[must_use]
pub fn build_statx_sqe_unchecked(args: &mut StatxArgs<'_>) -> squeue::Entry {
    opcode::Statx::new(
        types::Fd(args.dirfd),
        args.pathname.as_ptr(),
        (args.statx_buf as *mut rustix::fs::Statx).cast::<types::statx>(),
    )
    .flags(args.flags)
    .mask(args.mask)
    .build()
}

/// Synchronously submits a `STATX` SQE on a private throwaway ring and
/// returns the kernel's statx result.
///
/// This is the high-level convenience wrapper around [`build_statx_sqe`]
/// for callers that do not maintain a long-lived ring (tests, one-shot
/// stat calls in slow paths). The function builds a 2-entry ring,
/// submits the SQE, blocks for one completion, and returns:
///
/// - `Ok(statx)` on success - the statx buffer was populated.
/// - `Err(Unsupported)` when the kernel does not advertise
///   `IORING_OP_STATX`.
/// - `Err(io::Error::from_raw_os_error(-result))` when the kernel returns a
///   negative errno through the CQE.
///
/// `args` borrows the `CStr` path and the statx buffer; the function keeps
/// them alive for the duration of the syscall.
#[allow(unsafe_code)]
pub fn submit_statx_blocking(
    dirfd: i32,
    pathname: &CStr,
    flags: i32,
    mask: u32,
) -> io::Result<rustix::fs::Statx> {
    // SAFETY: `Statx` is a plain POSIX struct of integer fields, so the
    // all-zero bit pattern is a valid (if uninteresting) value that the
    // kernel will overwrite during the statx call.
    let mut statx_buf: rustix::fs::Statx = unsafe { std::mem::zeroed() };
    {
        let mut args = StatxArgs {
            dirfd,
            pathname,
            flags,
            mask,
            statx_buf: &mut statx_buf,
        };
        let sqe = build_statx_sqe(&mut args)?.user_data(0);
        let mut ring = io_uring::IoUring::new(2)?;
        // SAFETY: `sqe` references the CStr path borrowed from `pathname` and
        // the statx buffer borrowed from our stack, both of which outlive
        // `submit_and_wait`. The ring is local and has no other outstanding
        // references.
        unsafe {
            ring.submission()
                .push(&sqe)
                .map_err(|_| io::Error::other("submission queue full while pushing STATX SQE"))?;
        }
        ring.submit_and_wait(1)?;
        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("missing STATX CQE"))?;
        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }
    }
    Ok(statx_buf)
}

/// Result of a single statx operation within a batch.
///
/// On success, contains the populated [`rustix::fs::Statx`] struct. On
/// failure, contains the I/O error from the kernel or the fallback path.
pub type StatxResult = io::Result<rustix::fs::Statx>;

/// Submits multiple `IORING_OP_STATX` operations as independent SQEs on a
/// single io_uring instance and returns all results.
///
/// When `IORING_OP_STATX` is supported, each path in `paths` is submitted
/// as an independent SQE (not linked) on a shared ring. The ring processes
/// them concurrently, amortizing the syscall overhead across the entire
/// batch. Results are returned in the same order as the input paths.
///
/// When `IORING_OP_STATX` is not supported (kernel < 5.11, non-Linux,
/// or io_uring unavailable), falls back to synchronous `statx(2)` via
/// `rustix::fs::statx` for each path.
///
/// # Arguments
///
/// * `paths` - Slice of paths to stat.
/// * `follow_symlinks` - If `true`, follows symlinks (like `stat`);
///   if `false`, does not follow (like `lstat`).
///
/// # Returns
///
/// A `Vec<StatxResult>` with one entry per input path, preserving order.
/// Individual entries may be `Err` (e.g., `ENOENT`) without affecting
/// other entries in the batch.
///
/// # Errors
///
/// Returns `Err` only for ring-level failures (ring creation, submission
/// failure). Per-path errors are captured in individual `StatxResult` entries.
pub fn submit_statx_batch(paths: &[&Path], follow_symlinks: bool) -> io::Result<Vec<StatxResult>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    if !statx_supported() {
        return Ok(fallback_statx_batch(paths, follow_symlinks));
    }

    submit_statx_batch_io_uring(paths, follow_symlinks)
}

/// io_uring batch submission path. Submits all paths as independent SQEs.
#[allow(unsafe_code)]
fn submit_statx_batch_io_uring(
    paths: &[&Path],
    follow_symlinks: bool,
) -> io::Result<Vec<StatxResult>> {
    use std::ffi::CString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    let count = paths.len();
    // Round SQ depth up to the next power of two, clamped to at least 4.
    let sq_depth = (count as u32).next_power_of_two().max(4);
    let mut ring = io_uring::IoUring::new(sq_depth)?;

    let flags: i32 = if follow_symlinks {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    let mask: u32 = rustix::fs::StatxFlags::BASIC_STATS.bits();

    // Allocate CString paths and statx buffers upfront so they stay alive
    // across submission and completion.
    let mut c_paths: Vec<Option<CString>> = Vec::with_capacity(count);
    // SAFETY: `Statx` is a POD struct of integer fields; an all-zero bit
    // pattern is valid, and the kernel overwrites every populated entry.
    let mut statx_bufs: Vec<rustix::fs::Statx> = vec![unsafe { std::mem::zeroed() }; count];
    let mut path_errors: Vec<Option<io::Error>> = (0..count).map(|_| None).collect();

    for (i, path) in paths.iter().enumerate() {
        match CString::new(path.as_os_str().as_bytes()) {
            Ok(c) => c_paths.push(Some(c)),
            Err(_) => {
                c_paths.push(None);
                path_errors[i] = Some(io::Error::other("path contains interior NUL"));
            }
        }
    }

    // Submit in chunks up to the SQ depth to avoid overflowing the ring.
    let chunk_size = sq_depth as usize;
    let mut submitted_indices: Vec<usize> = Vec::with_capacity(count);

    for chunk_start in (0..count).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(count);
        submitted_indices.clear();

        for i in chunk_start..chunk_end {
            if path_errors[i].is_some() || c_paths[i].is_none() {
                continue;
            }

            let c_path = c_paths[i].as_ref().unwrap();
            let sqe = opcode::Statx::new(
                types::Fd(libc::AT_FDCWD),
                c_path.as_ptr(),
                (&mut statx_bufs[i] as *mut rustix::fs::Statx).cast::<types::statx>(),
            )
            .flags(flags)
            .mask(mask)
            .build()
            .user_data(i as u64);

            // SAFETY: The CString paths and statx buffers live in Vecs that
            // outlive the ring submission and completion. The ring is local
            // to this function. Each SQE references a distinct CString and
            // statx buffer slot, so there are no aliasing violations.
            unsafe {
                ring.submission().push(&sqe).map_err(|_| {
                    io::Error::other("submission queue full while pushing STATX SQE")
                })?;
            }
            submitted_indices.push(i);
        }

        if submitted_indices.is_empty() {
            continue;
        }

        let want = submitted_indices.len();
        ring.submit_and_wait(want)?;

        // Reap all completions for this chunk.
        let mut reaped = 0;
        for cqe in ring.completion() {
            let idx = cqe.user_data() as usize;
            let result = cqe.result();
            if result < 0 {
                path_errors[idx] = Some(io::Error::from_raw_os_error(-result));
            }
            reaped += 1;
        }

        if reaped < want {
            return Err(io::Error::other(format!(
                "expected {want} CQEs but only reaped {reaped}"
            )));
        }
    }

    // Assemble results in input order.
    let mut results = Vec::with_capacity(count);
    for i in 0..count {
        if let Some(err) = path_errors[i].take() {
            results.push(Err(err));
        } else {
            results.push(Ok(statx_bufs[i]));
        }
    }

    Ok(results)
}

/// Synchronous fallback path using `rustix::fs::statx` for each path.
///
/// Used when `IORING_OP_STATX` is not available (kernel < 5.11, non-Linux,
/// or io_uring disabled).
fn fallback_statx_batch(paths: &[&Path], follow_symlinks: bool) -> Vec<StatxResult> {
    paths
        .iter()
        .map(|path| fallback_statx_single(path, follow_symlinks))
        .collect()
}

/// Single-path synchronous statx via `rustix`.
fn fallback_statx_single(path: &Path, follow_symlinks: bool) -> StatxResult {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{AtFlags, StatxFlags};

        let flags = if follow_symlinks {
            AtFlags::empty()
        } else {
            AtFlags::SYMLINK_NOFOLLOW
        };
        let mask = StatxFlags::BASIC_STATS;

        rustix::fs::statx(rustix::fs::CWD, path, flags, mask)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, follow_symlinks);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "statx is not available on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statx_opcode_constant_matches_kernel_uapi() {
        // IORING_OP_STATX == 21 in include/uapi/linux/io_uring.h since 5.11.
        assert_eq!(IORING_OP_STATX, 21);
        assert_eq!(opcode::Statx::CODE, IORING_OP_STATX);
    }

    #[test]
    fn statx_min_kernel_is_5_11() {
        assert_eq!(STATX_MIN_KERNEL, (5, 11));
    }

    #[test]
    fn statx_supported_is_idempotent() {
        let first = statx_supported();
        let second = statx_supported();
        let third = statx_supported();
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn statx_supported_implies_io_uring_available() {
        if statx_supported() {
            assert!(is_io_uring_available());
        }
    }

    /// Convenience: rustix's `BASIC_STATS` bitflag value as a raw `u32`.
    fn basic_stats_mask() -> u32 {
        rustix::fs::StatxFlags::BASIC_STATS.bits()
    }

    #[test]
    fn build_statx_sqe_returns_unsupported_when_probe_false() {
        if statx_supported() {
            return; // probe says yes; cannot exercise the unsupported branch
        }
        let path = c"/tmp/test";
        // SAFETY: `Statx` is POD; zeroing yields a valid placeholder buffer
        // that the SQE builder treats as a write destination.
        let mut buf: rustix::fs::Statx = unsafe { std::mem::zeroed() };
        let err = build_statx_sqe(&mut StatxArgs {
            dirfd: libc::AT_FDCWD,
            pathname: path,
            flags: 0,
            mask: basic_stats_mask(),
            statx_buf: &mut buf,
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn build_statx_sqe_unchecked_smoke() {
        let path = c"/tmp/test";
        // SAFETY: `Statx` is POD; zeroing yields a valid placeholder buffer
        // that the SQE builder treats as a write destination.
        let mut buf: rustix::fs::Statx = unsafe { std::mem::zeroed() };
        // Construct without panic and tag with user_data.
        let _tagged = build_statx_sqe_unchecked(&mut StatxArgs {
            dirfd: libc::AT_FDCWD,
            pathname: path,
            flags: libc::AT_SYMLINK_NOFOLLOW,
            mask: basic_stats_mask(),
            statx_buf: &mut buf,
        })
        .user_data(0xCAFE_F00D);
    }

    #[test]
    fn build_statx_sqe_succeeds_when_probe_true() {
        if !statx_supported() {
            return;
        }
        let path = c"/tmp/test";
        // SAFETY: `Statx` is POD; zeroing yields a valid placeholder buffer
        // that the SQE builder treats as a write destination.
        let mut buf: rustix::fs::Statx = unsafe { std::mem::zeroed() };
        let _entry = build_statx_sqe(&mut StatxArgs {
            dirfd: libc::AT_FDCWD,
            pathname: path,
            flags: 0,
            mask: basic_stats_mask(),
            statx_buf: &mut buf,
        })
        .expect("probe true implies SQE builds")
        .user_data(0x1234);
    }

    #[test]
    fn submit_statx_blocking_on_existing_file() {
        if !statx_supported() {
            return;
        }
        // /proc/self/exe always exists on Linux.
        let path = c"/proc/self/exe";
        let result = submit_statx_blocking(libc::AT_FDCWD, path, 0, basic_stats_mask());
        let statx_buf = result.expect("statx on /proc/self/exe should succeed");
        // The file must be a regular file or symlink.
        assert!(
            u32::from(statx_buf.stx_mode) & libc::S_IFMT == libc::S_IFREG
                || u32::from(statx_buf.stx_mode) & libc::S_IFMT == libc::S_IFLNK,
            "expected regular file or symlink, got mode {:#o}",
            statx_buf.stx_mode
        );
    }

    #[test]
    fn submit_statx_blocking_on_nonexistent_file() {
        if !statx_supported() {
            return;
        }
        let path = c"/nonexistent/path/that/does/not/exist";
        let err = submit_statx_blocking(libc::AT_FDCWD, path, 0, basic_stats_mask()).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    }

    #[test]
    fn submit_statx_blocking_lstat_mode() {
        if !statx_supported() {
            return;
        }
        let path = c"/proc/self/exe";
        let result = submit_statx_blocking(
            libc::AT_FDCWD,
            path,
            libc::AT_SYMLINK_NOFOLLOW,
            basic_stats_mask(),
        );
        // /proc/self/exe is a symlink; with AT_SYMLINK_NOFOLLOW it should
        // report as a symlink.
        let statx_buf = result.expect("lstat on /proc/self/exe should succeed");
        assert_eq!(
            u32::from(statx_buf.stx_mode) & libc::S_IFMT,
            libc::S_IFLNK,
            "expected symlink, got mode {:#o}",
            statx_buf.stx_mode
        );
    }

    #[test]
    fn submit_statx_batch_empty() {
        let paths: &[&Path] = &[];
        let results = submit_statx_batch(paths, true).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn submit_statx_batch_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let paths: Vec<&Path> = vec![file_path.as_path()];
        let results = submit_statx_batch(&paths, true).unwrap();
        assert_eq!(results.len(), 1);

        let statx_buf = results.into_iter().next().unwrap().unwrap();
        assert_eq!(statx_buf.stx_size, 5);
        assert_eq!(
            u32::from(statx_buf.stx_mode) & libc::S_IFMT,
            libc::S_IFREG,
            "expected regular file"
        );
    }

    #[test]
    fn submit_statx_batch_multiple_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths_owned: Vec<_> = (0..16)
            .map(|i| {
                let p = dir.path().join(format!("file_{i}.txt"));
                std::fs::write(&p, format!("content {i}")).unwrap();
                p
            })
            .collect();
        let paths: Vec<&Path> = paths_owned.iter().map(|p| p.as_path()).collect();

        let results = submit_statx_batch(&paths, true).unwrap();
        assert_eq!(results.len(), 16);

        for (i, result) in results.into_iter().enumerate() {
            let statx_buf = result.unwrap_or_else(|e| panic!("statx failed for file_{i}: {e}"));
            let expected_content = format!("content {i}");
            assert_eq!(
                statx_buf.stx_size,
                expected_content.len() as u64,
                "size mismatch for file_{i}"
            );
        }
    }

    #[test]
    fn submit_statx_batch_with_errors() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("exists.txt");
        std::fs::write(&existing, b"data").unwrap();
        let missing = dir.path().join("does_not_exist.txt");

        let paths: Vec<&Path> = vec![existing.as_path(), missing.as_path()];
        let results = submit_statx_batch(&paths, true).unwrap();
        assert_eq!(results.len(), 2);

        // First should succeed.
        assert!(results[0].is_ok());
        // Second should fail with ENOENT.
        let err = results[1].as_ref().unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    }

    #[test]
    fn submit_statx_batch_preserves_order() {
        let dir = tempfile::tempdir().unwrap();
        let sizes = [10u64, 20, 30, 40, 50];
        let paths_owned: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                let p = dir.path().join(format!("ordered_{i}.bin"));
                std::fs::write(&p, vec![0u8; size as usize]).unwrap();
                p
            })
            .collect();
        let paths: Vec<&Path> = paths_owned.iter().map(|p| p.as_path()).collect();

        let results = submit_statx_batch(&paths, true).unwrap();
        assert_eq!(results.len(), sizes.len());

        for (i, (result, &expected_size)) in results.iter().zip(sizes.iter()).enumerate() {
            let statx_buf = result
                .as_ref()
                .unwrap_or_else(|e| panic!("statx failed for ordered_{i}: {e}"));
            assert_eq!(
                statx_buf.stx_size, expected_size,
                "order mismatch at index {i}"
            );
        }
    }

    #[test]
    fn submit_statx_batch_symlink_follow_vs_nofollow() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"symlink target").unwrap();

        #[cfg(unix)]
        {
            let link = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            // Following symlinks - should see regular file.
            let paths: Vec<&Path> = vec![link.as_path()];
            let results_follow = submit_statx_batch(&paths, true).unwrap();
            let statx_follow = results_follow[0].as_ref().unwrap();
            assert_eq!(
                u32::from(statx_follow.stx_mode) & libc::S_IFMT,
                libc::S_IFREG,
                "expected regular file when following symlinks"
            );

            // Not following symlinks - should see symlink.
            let results_nofollow = submit_statx_batch(&paths, false).unwrap();
            let statx_nofollow = results_nofollow[0].as_ref().unwrap();
            assert_eq!(
                u32::from(statx_nofollow.stx_mode) & libc::S_IFMT,
                libc::S_IFLNK,
                "expected symlink when not following symlinks"
            );
        }
    }

    #[test]
    fn kernel_version_probe_is_consistent_with_min_kernel() {
        // If statx is supported, the kernel must be >= 5.11.
        if statx_supported() {
            let release = super::super::config::config_detail::get_kernel_release_string();
            if let Some(release) = release {
                if let Some((major, minor)) =
                    super::super::config::config_detail::parse_kernel_version(&release)
                {
                    assert!(
                        (major, minor) >= STATX_MIN_KERNEL,
                        "statx probe says supported but kernel {major}.{minor} < 5.11"
                    );
                }
            }
        }
    }

    #[test]
    fn fallback_statx_batch_produces_results() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("fallback_test.txt");
        std::fs::write(&file_path, b"fallback").unwrap();

        let paths: Vec<&Path> = vec![file_path.as_path()];
        let results = fallback_statx_batch(&paths, true);
        assert_eq!(results.len(), 1);

        #[cfg(target_os = "linux")]
        {
            let statx_buf = results.into_iter().next().unwrap().unwrap();
            assert_eq!(statx_buf.stx_size, 8);
        }

        #[cfg(not(target_os = "linux"))]
        {
            assert!(results[0].is_err());
        }
    }
}
