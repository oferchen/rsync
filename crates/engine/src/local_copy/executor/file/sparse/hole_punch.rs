//! Filesystem hole punching via `fallocate(PUNCH_HOLE)`.
//!
//! On Linux, uses `fallocate(2)` with `FALLOC_FL_PUNCH_HOLE` to deallocate
//! blocks for zero-filled regions. Falls back to writing explicit zeros on
//! platforms without hole-punch support.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

#[cfg(target_os = "linux")]
use rustix::{
    fd::AsFd,
    fs::{FallocateFlags, fallocate},
    io::Errno,
};

use crate::local_copy::LocalCopyError;

use super::ZERO_WRITE_BUFFER_SIZE;

/// Punches a hole in the file at the specified position for the given length.
///
/// Mirrors upstream rsync's `do_punch_hole()` function (syscall.c) with a
/// three-tier fallback strategy:
///
/// 1. Try `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE` - creates actual hole
/// 2. Fall back to `FALLOC_FL_ZERO_RANGE` - zeroes range without allocation
/// 3. Final fallback: write zeros - universal but dense
///
/// After a successful call, the file position will be at `pos + len`.
///
/// # Arguments
///
/// * `file` - The file to punch holes in
/// * `path` - Path for error reporting
/// * `pos` - Starting position for the hole
/// * `len` - Length of the hole in bytes
///
/// Not yet wired into the production delta transfer path (test-only).
/// See task #677 for in-place update integration.
#[cfg(target_os = "linux")]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn punch_hole(
    file: &mut fs::File,
    path: &Path,
    pos: u64,
    len: u64,
) -> Result<(), LocalCopyError> {
    if len == 0 {
        return Ok(());
    }

    // Ensure position doesn't exceed i64::MAX for fallocate
    if pos > i64::MAX as u64 || len > i64::MAX as u64 {
        return write_zeros_fallback(file, path, len);
    }

    let fd = file.as_fd();

    // Strategy 1: Try PUNCH_HOLE | KEEP_SIZE (creates actual filesystem hole)
    let punch_flags = FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE;
    match fallocate(fd, punch_flags, pos, len) {
        Ok(()) => {
            // Seek to pos + len after successful hole punch
            file.seek(SeekFrom::Start(pos + len))
                .map_err(|e| LocalCopyError::io("seek after hole punch", path, e))?;
            return Ok(());
        }
        Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
            // PUNCH_HOLE not supported, try ZERO_RANGE
        }
        Err(_errno) => {
            // Unexpected error, fall through to ZERO_RANGE fallback
        }
    }

    // Strategy 2: Try ZERO_RANGE (zeroes range without allocation on some systems)
    match fallocate(fd, FallocateFlags::ZERO_RANGE, pos, len) {
        Ok(()) => {
            // Seek to pos + len after successful zero range
            file.seek(SeekFrom::Start(pos + len))
                .map_err(|e| LocalCopyError::io("seek after zero range", path, e))?;
            return Ok(());
        }
        Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
            // ZERO_RANGE not supported, fall back to writing zeros
        }
        Err(_errno) => {
            // Unexpected error, fall through to write zeros
        }
    }

    // Strategy 3: Write zeros (universal but allocates space)
    write_zeros_fallback(file, path, len)
}

/// Non-Linux platforms fall back to writing zeros directly.
/// This includes macOS, BSD, and Windows which don't support Linux's
/// fallocate PUNCH_HOLE/ZERO_RANGE flags.
///
/// After a successful call, the file position will be at `pos + len`,
/// matching the Linux implementation's behavior.
///
/// Not yet wired into the production delta transfer path (test-only).
/// See task #677 for in-place update integration.
#[cfg(not(target_os = "linux"))]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn punch_hole(
    file: &mut fs::File,
    path: &Path,
    pos: u64,
    len: u64,
) -> Result<(), LocalCopyError> {
    if len == 0 {
        return Ok(());
    }

    // Seek to the starting position before writing zeros
    file.seek(SeekFrom::Start(pos))
        .map_err(|e| LocalCopyError::io("seek before writing zeros", path, e))?;

    write_zeros_fallback(file, path, len)
}

/// Writes zeros to fill the specified length.
///
/// This is the final fallback when fallocate-based hole punching is not
/// available. Unlike hole punching, this allocates disk space.
pub(super) fn write_zeros_fallback(
    file: &mut fs::File,
    path: &Path,
    mut len: u64,
) -> Result<(), LocalCopyError> {
    let zeros = [0u8; ZERO_WRITE_BUFFER_SIZE];

    while len > 0 {
        let chunk_size = len.min(ZERO_WRITE_BUFFER_SIZE as u64) as usize;
        file.write_all(&zeros[..chunk_size])
            .map_err(|e| LocalCopyError::io("write zeros for sparse hole", path, e))?;
        len -= chunk_size as u64;
    }

    Ok(())
}
