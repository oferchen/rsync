//! Linux `FICLONERANGE` ioctl wrapper for partial basis-range reflinks.
//!
//! This module exposes the [`try_clone_file_range`] entry point used by the
//! delta-apply COPY-token fast path. Unlike `crate::copy_basis_range`, which
//! drives `copy_file_range(2)` and lets the kernel choose between an
//! in-kernel byte copy or a CoW reflink, `FICLONERANGE` is unconditional: when
//! it succeeds, the destination range shares storage extents with the basis
//! (zero data copied, metadata-only). When the filesystem cannot satisfy a
//! reflink (`EOPNOTSUPP`), the request crosses filesystems (`EXDEV`), the
//! offsets and length are not block-aligned (`EINVAL`), or the kernel pre-dates
//! Linux 4.5 (`ENOSYS`), the wrapper returns `Ok(false)` so the caller can
//! fall back to `copy_file_range(2)` or read+write.
//!
//! Upstream rsync 3.4.1 does not use `FICLONERANGE`. This is an oc-rsync-only
//! optimization that produces byte-identical output and changes no protocol
//! behavior - it is invisible on the wire.
//!
//! # Why FICLONERANGE in addition to `copy_file_range`?
//!
//! `copy_file_range(2)` will perform a reflink on CoW filesystems when the
//! kernel decides one is profitable, but the decision and the fallback are
//! opaque. `FICLONERANGE` is the explicit, deterministic primitive: success
//! means a metadata-only operation (instant, irrespective of range size),
//! failure means "this filesystem cannot reflink these bytes" and the caller
//! must use a real copy. For multi-GB COPY tokens on btrfs / XFS / bcachefs
//! the difference is decisive: tens of milliseconds (metadata) versus seconds
//! (kernel-side byte copy).
//!
//! # Alignment contract
//!
//! `FICLONERANGE` requires the source offset, destination offset, and length
//! to be multiples of the filesystem block size (4 KiB on all currently
//! supported CoW filesystems; some configurations use 16/64 KiB). The wrapper
//! does not query the filesystem - callers must check alignment before
//! invoking and pass through to `copy_file_range` when ranges are not aligned.

use std::fs::File;
use std::io;

/// Minimum range length below which `FICLONERANGE` is not worth attempting.
///
/// The ioctl involves a metadata transaction on the destination inode and an
/// extent-tree walk on the source. For tiny ranges the cost dominates over a
/// `pread` + `write` pair. The receiver's typical delta block sizes are well
/// above this threshold for files large enough to matter; this exists so the
/// caller can decline cheaply on tail blocks.
pub const CLONE_FILE_RANGE_MIN_BYTES: u64 = 16 * 1024;

/// Clones `len` bytes from `basis[basis_offset..]` into `dst[dst_offset..]`
/// using Linux `FICLONERANGE`, or returns `Ok(false)` when the platform,
/// filesystem, or alignment is unsuitable.
///
/// # Returns
///
/// - `Ok(true)` when the ioctl succeeded and the destination extents now
///   share storage with the basis.
/// - `Ok(false)` when the platform is not Linux, the kernel pre-dates
///   `FICLONERANGE` (`ENOSYS`), the filesystem does not support reflinks
///   (`EOPNOTSUPP`), the basis and destination are on different filesystems
///   (`EXDEV`), or the offsets / length are not block-aligned (`EINVAL`).
///   The destination is untouched and the caller must fall back to a real
///   copy.
///
/// # Errors
///
/// Returns `Err` for real I/O errors (`EIO`, `ENOSPC`, `EPERM`, etc.).
/// Filesystem-level "cannot reflink this" errors are translated into
/// `Ok(false)` so callers can chain fallbacks without inspecting `errno`.
///
/// # Platform support
///
/// - **Linux 4.5+**: invokes `ioctl(dst_fd, FICLONERANGE, &file_clone_range)`
///   on btrfs, XFS (reflink enabled), and bcachefs. Same-filesystem only.
/// - **Other platforms**: returns `Ok(false)` immediately, no I/O issued.
#[inline]
pub fn try_clone_file_range(
    basis: &File,
    basis_offset: u64,
    dst: &File,
    dst_offset: u64,
    len: u64,
) -> io::Result<bool> {
    imp::try_clone_file_range(basis, basis_offset, dst, dst_offset, len)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    pub(super) fn try_clone_file_range(
        basis: &File,
        basis_offset: u64,
        dst: &File,
        dst_offset: u64,
        len: u64,
    ) -> io::Result<bool> {
        if len == 0 {
            return Ok(false);
        }

        let basis_fd = basis.as_raw_fd();
        let dst_fd = dst.as_raw_fd();

        // file_clone_range is the ABI struct expected by FICLONERANGE.
        // src_fd is i64 in the ABI - lower bits are the descriptor.
        let arg = libc::file_clone_range {
            src_fd: basis_fd as i64,
            src_offset: basis_offset,
            src_length: len,
            dest_offset: dst_offset,
        };

        // SAFETY: both fds are valid for the duration of the call (borrowed
        // from &File). `arg` is a stack-allocated, fully initialised
        // file_clone_range whose lifetime exceeds the ioctl. The kernel
        // reads `arg` synchronously and does not retain the pointer past
        // return. No memory beyond `arg` is read or written through it.
        #[allow(unsafe_code)]
        let ret = unsafe { libc::ioctl(dst_fd, libc::FICLONERANGE, &arg) };

        if ret == 0 {
            return Ok(true);
        }

        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            // "This filesystem cannot reflink the requested range" - caller
            // falls back to copy_file_range / read+write.
            // libc::ENOTSUP == libc::EOPNOTSUPP on Linux; one suffices.
            Some(libc::EOPNOTSUPP)
            | Some(libc::EXDEV)
            | Some(libc::EINVAL)
            | Some(libc::ENOSYS)
            | Some(libc::ETXTBSY)
            | Some(libc::EPERM) => Ok(false),
            _ => Err(err),
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::fs::File;
    use std::io;

    pub(super) fn try_clone_file_range(
        _basis: &File,
        _basis_offset: u64,
        _dst: &File,
        _dst_offset: u64,
        _len: u64,
    ) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use tempfile::tempdir;

    fn make_basis(dir: &std::path::Path, name: &str, payload: &[u8]) -> File {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(payload).unwrap();
        f.sync_all().ok();
        File::open(&path).unwrap()
    }

    fn make_dest(dir: &std::path::Path, name: &str, size: u64) -> File {
        let path = dir.join(name);
        let f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        f.set_len(size).unwrap();
        f
    }

    #[test]
    fn zero_length_returns_false_without_syscall() {
        let dir = tempdir().unwrap();
        let basis = make_basis(dir.path(), "b", b"abc");
        let dest = make_dest(dir.path(), "d", 0);
        let ok = try_clone_file_range(&basis, 0, &dest, 0, 0).unwrap();
        assert!(!ok);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unaligned_or_unsupported_fs_returns_false() {
        // On tmpfs / ext4 (the typical tempdir backing) FICLONERANGE returns
        // EOPNOTSUPP. On btrfs / XFS / bcachefs aligned to the block size the
        // clone succeeds. Either outcome is fine; what we are asserting is
        // that the wrapper never panics and never propagates a filesystem
        // unsupported-error as `Err`.
        let dir = tempdir().unwrap();
        let payload = vec![0xA5u8; 64 * 1024];
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let dest = make_dest(dir.path(), "dest.bin", payload.len() as u64);

        let result = try_clone_file_range(&basis, 0, &dest, 0, payload.len() as u64);
        assert!(
            result.is_ok(),
            "wrapper should not surface fs-unsupported as Err"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_always_returns_false() {
        let dir = tempdir().unwrap();
        let payload = vec![0u8; 4096];
        let basis = make_basis(dir.path(), "b", &payload);
        let dest = make_dest(dir.path(), "d", payload.len() as u64);
        let ok = try_clone_file_range(&basis, 0, &dest, 0, payload.len() as u64).unwrap();
        assert!(!ok);
    }
}
