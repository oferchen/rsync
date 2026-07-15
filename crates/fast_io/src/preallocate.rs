//! Destination file preallocation via `fallocate(FALLOC_FL_KEEP_SIZE)`.
//!
//! On Linux, reserves disk blocks for a file's eventual length before writing
//! begins, reducing fragmentation on extent-based filesystems (ext4, xfs, NTFS).
//! Mirrors upstream rsync's `do_fallocate()` (`syscall.c`), which the receiver
//! calls when `--preallocate` is set. On platforms without `fallocate` upstream
//! compiles the preallocation path out (`SUPPORT_PREALLOCATION` undefined), so
//! this is a no-op there.

use std::fs::File;
use std::io;

#[cfg(target_os = "linux")]
use rustix::fs::{FallocateFlags, fallocate};

/// Preallocates disk blocks for `file`'s eventual `length` bytes without
/// changing the file's logical size.
///
/// Returns the number of already-reserved bytes the caller should treat as
/// `preallocated_len`: with `FALLOC_FL_KEEP_SIZE` the file size is unchanged so
/// this is `0`, exactly as upstream `do_fallocate()` returns `0` for its
/// `opts != 0` case. A zero `length` (or one that overflows the signed syscall
/// argument) is a no-op returning `0`.
///
/// # Errors
///
/// Returns the underlying `fallocate` error. Callers must mirror upstream's
/// `rsyserr(FWARNING, ...)` and continue: preallocation is an optimization, and
/// its failure (an unsupported filesystem, `ENOSPC`, ...) must never abort the
/// transfer.
// upstream: syscall.c:1528 do_fallocate() / receiver.c:323
#[cfg(target_os = "linux")]
pub fn preallocate(file: &File, length: u64) -> io::Result<u64> {
    // fallocate's offset/length args are signed; skip when length would
    // overflow i64 (also covers the `length + 1` perturbation below).
    if length == 0 || length >= i64::MAX as u64 {
        return Ok(0);
    }
    // upstream: syscall.c:1534-1537 - perturb the length by one so it never
    // exactly matches the file's eventual size (only observable on the
    // KEEP_SIZE-unavailable fallback, but replicated for fidelity).
    let length = if length & 1 == 1 {
        length + 1
    } else {
        length - 1
    };
    // upstream: syscall.c:1530 - opts == FALLOC_FL_KEEP_SIZE when preallocating.
    match fallocate(file, FallocateFlags::KEEP_SIZE, 0, length) {
        // upstream: syscall.c:1555 - opts != 0 returns 0 (size unchanged).
        Ok(()) => Ok(0),
        Err(err) => Err(io::Error::from_raw_os_error(err.raw_os_error())),
    }
}

/// Non-Linux platforms lack `fallocate`; upstream compiles the preallocation
/// path out entirely (`SUPPORT_PREALLOCATION` undefined), so this is a no-op
/// that reports no reserved extent.
// upstream: syscall.c - SUPPORT_PREALLOCATION guards the whole feature.
#[cfg(not(target_os = "linux"))]
pub fn preallocate(_file: &File, _length: u64) -> io::Result<u64> {
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preallocate_zero_length_is_noop() {
        let f = tempfile::tempfile().expect("tempfile");
        assert_eq!(preallocate(&f, 0).expect("prealloc"), 0);
    }

    #[test]
    fn preallocate_absurd_length_is_noop() {
        let f = tempfile::tempfile().expect("tempfile");
        assert_eq!(preallocate(&f, u64::MAX).expect("prealloc"), 0);
    }

    // Preallocation reserves blocks up front so the eventual write lands in one
    // contiguous extent; verify st_blocks reflects the reservation before any
    // bytes are written. Linux-only: fallocate is the only tier that reserves
    // without changing the logical size. Degrades gracefully if the temp
    // filesystem (e.g. tmpfs) does not support fallocate.
    #[cfg(target_os = "linux")]
    #[test]
    fn preallocate_reserves_blocks_without_growing_size() {
        use std::os::unix::fs::MetadataExt;

        let f = tempfile::tempfile().expect("tempfile");
        let one_mib: u64 = 1024 * 1024;
        match preallocate(&f, one_mib) {
            Ok(len) => {
                // KEEP_SIZE reports 0 reserved-for-punching bytes.
                assert_eq!(len, 0, "KEEP_SIZE preallocation reports 0");
                let meta = f.metadata().expect("metadata");
                // Logical size stays 0 (KEEP_SIZE); blocks are reserved.
                assert_eq!(meta.len(), 0, "KEEP_SIZE must not grow logical size");
                let reserved = meta.blocks() * 512;
                assert!(
                    reserved + 512 >= one_mib,
                    "expected ~{one_mib} bytes reserved, got {reserved}"
                );
            }
            // tmpfs and other filesystems reject fallocate; the transfer must
            // still succeed, so a rejection here is an acceptable degrade.
            Err(err) => {
                let raw = err.raw_os_error();
                assert!(
                    matches!(raw, Some(libc::EOPNOTSUPP | libc::ENOSYS | libc::ENOSPC)),
                    "unexpected preallocate error: {err}"
                );
            }
        }
    }
}
