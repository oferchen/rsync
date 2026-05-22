//! Offset-aware basis-to-destination range copy via `copy_file_range(2)`.
//!
//! This module is the IUD-10 fast path for the delta-apply COPY-token branch:
//! when the receiver replays a `COPY {index, len}` token, the contents at
//! `basis[basis_off..basis_off+len]` must end up at
//! `dest[dest_off..dest_off+len]`. On Linux 4.5+ with both files on the same
//! filesystem, `copy_file_range(2)` accomplishes this entirely in the kernel
//! without bouncing bytes through userspace - no per-byte `read(2)`/`write(2)`
//! and, on supporting filesystems, with reflink/server-side copy acceleration.
//!
//! Upstream rsync 3.4.1 itself does not use `copy_file_range(2)` in the
//! receiver - it walks `map_ptr` and `write(2)`. This is an oc-rsync
//! optimization that produces byte-identical output; it is invisible on the
//! wire and changes no protocol behavior.
//!
//! The wrapper is conservative: any kernel-reported failure on the first
//! iteration collapses to `Ok(0)` so callers fall back to the existing
//! read-then-write path without surfacing platform-specific errnos. Only
//! failures after a partial copy succeed are propagated, because at that
//! point the destination is in a state the caller must reconcile.
//!
//! # Bounded I/O contract
//!
//! Every iteration submits at most `i64::MAX` bytes and the loop terminates
//! on three conditions: requested `len` reached, kernel returns 0 (EOF on the
//! source), or kernel returns an error. There is no unbounded retry; the
//! caller's `len` bounds total time.

use std::fs::File;
use std::io;

/// Minimum range size for which `copy_file_range` amortizes its syscall cost.
///
/// Smaller ranges are cheaper to satisfy with a single `pread`/`write` pair
/// than to round-trip through the syscall. The receiver's typical block size
/// is well above this threshold, so the dispatch is rarely declined on real
/// transfers; the threshold exists to keep tiny tail blocks fast.
pub const COPY_BASIS_RANGE_MIN_BYTES: usize = 4 * 1024;

/// Copies `len` bytes from `basis[basis_off..]` into `dest[dest_off..]` using
/// `copy_file_range(2)` on Linux, or returns `Ok(0)` on every other target
/// so callers fall back to read+write.
///
/// # Returns
///
/// - `Ok(n)` where `n == len` on full success.
/// - `Ok(n)` where `0 < n < len` if the kernel hit EOF on the basis before
///   the range was satisfied; the caller must reconcile the short copy
///   (rare with a correctly sized basis).
/// - `Ok(0)` when the syscall is unavailable, the files live on different
///   filesystems (`EXDEV`), the kernel does not support `copy_file_range`
///   (`ENOSYS`), or any first-iteration error. The caller must fall back to
///   the read+write path; the destination is untouched.
///
/// # Errors
///
/// Returns `Err` only when a kernel error occurs **after** a partial copy
/// has already succeeded. In that case the destination has been partially
/// written and the caller cannot transparently fall back.
///
/// # Bounded I/O
///
/// Caps each iteration at `i64::MAX` and terminates on completion, EOF, or
/// error - never hangs. Source and destination file offsets are NOT advanced
/// (the syscall uses explicit `loff_t*` pointers).
///
/// # Platform support
///
/// - Linux 4.5+ same filesystem.
/// - Linux 5.3+ cross filesystem (still gated to `Ok(0)` on `EXDEV` for
///   safety on older kernels).
/// - All other targets: returns `Ok(0)` immediately, no syscall issued.
#[inline]
pub fn copy_basis_range(
    basis: &File,
    basis_off: u64,
    dest: &File,
    dest_off: u64,
    len: usize,
) -> io::Result<usize> {
    imp::copy_basis_range(basis, basis_off, dest, dest_off, len)
}

/// Returns `true` when the running kernel exposes a usable
/// `copy_file_range(2)` syscall.
///
/// Result is cached via a process-wide `OnceLock` after the first probe so
/// subsequent dispatch decisions are branch-only. On non-Linux targets this
/// is a compile-time `false`.
#[must_use]
#[inline]
pub fn copy_file_range_supported() -> bool {
    imp::copy_file_range_supported()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::sync::OnceLock;

    static CFR_SUPPORTED: OnceLock<bool> = OnceLock::new();

    pub(super) fn copy_file_range_supported() -> bool {
        if let Some(cached) = CFR_SUPPORTED.get().copied() {
            return cached;
        }
        let result = probe();
        let _ = CFR_SUPPORTED.set(result);
        CFR_SUPPORTED.get().copied().unwrap_or(result)
    }

    fn probe() -> bool {
        // Issue a zero-byte copy_file_range against /dev/null in both
        // directions. A return of 0 with no error or any errno other than
        // ENOSYS means the syscall reached the kernel and is available;
        // ENOSYS means the kernel pre-dates Linux 4.5.
        let Ok(src) = File::open("/dev/null") else {
            return false;
        };
        let Ok(dst) = File::options().write(true).open("/dev/null") else {
            return false;
        };

        let mut src_off: libc::loff_t = 0;
        let mut dst_off: libc::loff_t = 0;
        // SAFETY: both fds are valid for the duration of this call (owned by
        // the local File values), the offset pointers are valid &mut local
        // variables, len=0 is a no-op probe documented to return 0 on
        // supported kernels. No memory is read or written.
        #[allow(unsafe_code)]
        let ret = unsafe {
            libc::copy_file_range(
                src.as_raw_fd(),
                &mut src_off as *mut libc::loff_t,
                dst.as_raw_fd(),
                &mut dst_off as *mut libc::loff_t,
                0,
                0,
            )
        };

        if ret >= 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS)
    }

    pub(super) fn copy_basis_range(
        basis: &File,
        basis_off: u64,
        dest: &File,
        dest_off: u64,
        len: usize,
    ) -> io::Result<usize> {
        if len == 0 {
            return Ok(0);
        }
        if !copy_file_range_supported() {
            return Ok(0);
        }

        let basis_fd = basis.as_raw_fd();
        let dest_fd = dest.as_raw_fd();

        let mut total: usize = 0;
        let mut src_off: libc::loff_t = match libc::loff_t::try_from(basis_off) {
            Ok(v) => v,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "basis_off exceeds loff_t",
                ));
            }
        };
        let mut dst_off: libc::loff_t = match libc::loff_t::try_from(dest_off) {
            Ok(v) => v,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "dest_off exceeds loff_t",
                ));
            }
        };

        while total < len {
            let remaining = len - total;
            let chunk = remaining.min(i64::MAX as usize);

            // SAFETY: both fds are valid for the call (borrowed from &File),
            // the loff_t pointers reference valid local mutables, and the
            // syscall does not retain any pointer past return. The kernel
            // advances *src_off and *dst_off in place by the number of bytes
            // copied.
            #[allow(unsafe_code)]
            let ret = unsafe {
                libc::copy_file_range(
                    basis_fd,
                    &mut src_off as *mut libc::loff_t,
                    dest_fd,
                    &mut dst_off as *mut libc::loff_t,
                    chunk,
                    0,
                )
            };

            if ret < 0 {
                let err = io::Error::last_os_error();
                if total == 0 {
                    // First iteration: caller falls back to read+write.
                    // EXDEV, EOPNOTSUPP, EINVAL (overlapping ranges, special
                    // files), and EXDEV-like errors all collapse to Ok(0).
                    return Ok(0);
                }
                return Err(err);
            }

            if ret == 0 {
                // EOF on basis before len was satisfied; short copy.
                break;
            }

            total += ret as usize;
        }

        Ok(total)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::fs::File;
    use std::io;

    pub(super) fn copy_file_range_supported() -> bool {
        false
    }

    pub(super) fn copy_basis_range(
        _basis: &File,
        _basis_off: u64,
        _dest: &File,
        _dest_off: u64,
        _len: usize,
    ) -> io::Result<usize> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use tempfile::tempdir;

    fn make_basis(dir: &std::path::Path, name: &str, payload: &[u8]) -> File {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(payload).unwrap();
        f.sync_all().ok();
        File::open(&path).unwrap()
    }

    fn make_dest(dir: &std::path::Path, name: &str) -> File {
        let path = dir.join(name);
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    }

    fn read_all(dest: &mut File) -> Vec<u8> {
        dest.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = Vec::new();
        dest.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn empty_len_returns_zero_without_syscall() {
        let dir = tempdir().unwrap();
        let basis = make_basis(dir.path(), "b", b"abc");
        let dest = make_dest(dir.path(), "d");
        let copied = copy_basis_range(&basis, 0, &dest, 0, 0).unwrap();
        assert_eq!(copied, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_fs_copy_produces_byte_identical_output() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let copied = copy_basis_range(&basis, 0, &dest, 0, payload.len()).unwrap();
        // On kernels without copy_file_range, copied == 0 and the caller
        // would fall back. On Linux 4.5+ with a tmpfs/ext4-backed tempdir,
        // expect a full or partial in-kernel copy.
        if copied == 0 {
            // Old kernel or restricted FS - production path falls back; the
            // wrapper contract is satisfied.
            return;
        }
        assert_eq!(copied, payload.len());

        let out = read_all(&mut dest);
        assert_eq!(out, payload);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn offset_copy_extracts_correct_window() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..8 * 1024).map(|i| (i % 211) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let window = 4 * 1024;
        let copied = copy_basis_range(&basis, 1024, &dest, 0, window).unwrap();
        if copied == 0 {
            return;
        }
        assert_eq!(copied, window);

        let out = read_all(&mut dest);
        assert_eq!(out, &payload[1024..1024 + window]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dest_offset_writes_at_correct_position() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..4 * 1024).map(|i| (i % 199) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");
        // Pre-fill destination with a sentinel so we can verify the write
        // landed at the requested offset and did not clobber earlier bytes.
        dest.write_all(&vec![0xFFu8; 2048]).unwrap();
        dest.sync_all().ok();

        let copied = copy_basis_range(&basis, 0, &dest, 2048, payload.len()).unwrap();
        if copied == 0 {
            return;
        }
        assert_eq!(copied, payload.len());

        let out = read_all(&mut dest);
        assert_eq!(&out[..2048], &vec![0xFFu8; 2048]);
        assert_eq!(&out[2048..], payload.as_slice());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn short_copy_when_basis_eof_returns_partial_count() {
        // Request more than the basis contains; the kernel returns 0 once it
        // hits EOF mid-loop. The wrapper exits the loop and reports the
        // bytes that did make it through - the receiver's checksum verifier
        // (when sequential) will then surface the mismatch.
        let dir = tempdir().unwrap();
        let payload = vec![0xA5u8; 2048];
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let copied = copy_basis_range(&basis, 0, &dest, 0, 8192).unwrap();
        if copied == 0 {
            return;
        }
        assert!(copied <= payload.len());
        assert!(copied >= 1);

        let out = read_all(&mut dest);
        assert_eq!(&out[..copied], &payload[..copied]);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_stub_returns_zero_without_syscall() {
        let dir = tempdir().unwrap();
        let basis = make_basis(dir.path(), "b", b"abcd");
        let dest = make_dest(dir.path(), "d");
        let copied = copy_basis_range(&basis, 0, &dest, 0, 4).unwrap();
        assert_eq!(copied, 0);
        assert!(!copy_file_range_supported());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supported_probe_is_stable_across_calls() {
        let a = copy_file_range_supported();
        let b = copy_file_range_supported();
        assert_eq!(a, b);
    }
}
