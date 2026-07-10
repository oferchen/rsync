//! Filesystem hole punching via `fallocate(PUNCH_HOLE)`.
//!
//! On Linux, uses `fallocate(2)` with `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`
//! to deallocate blocks for a zero-filled region while preserving the file's
//! logical length, then `FALLOC_FL_ZERO_RANGE` as a second tier. Both leave the
//! region reading back as zeros. On platforms without hole-punch support (and
//! when both `fallocate` tiers are rejected) it falls back to writing explicit
//! zeros, matching upstream rsync's `do_punch_hole()` fallback in `syscall.c`.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};

#[cfg(target_os = "linux")]
use rustix::{
    fs::{FallocateFlags, fallocate},
    io::Errno,
};

/// Buffer size for the zero-write fallback. Matches upstream rsync's
/// `do_punch_hole()` zero buffer sizing (`syscall.c`).
const ZERO_WRITE_BUFFER_SIZE: usize = 32 * 1024;

/// Punches a hole of `len` bytes at absolute offset `pos` in `file`, leaving the
/// region reading back as zeros without allocating disk blocks where possible.
///
/// The file's logical length is left unchanged (`FALLOC_FL_KEEP_SIZE`); callers
/// establish the final length with a separate `set_len`. On the zero-write
/// fallback the file position ends at `pos + len`; on the `fallocate` fast paths
/// the position is not moved (the caller does not depend on it).
///
/// Mirrors upstream rsync's `do_punch_hole()` (`syscall.c`).
///
/// # Errors
///
/// Returns an error if the zero-write fallback fails to seek or write.
#[cfg(target_os = "linux")]
pub fn punch_hole(file: &mut File, pos: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }

    // fallocate's offset/length args are signed; fall back when either would
    // overflow i64.
    if pos <= i64::MAX as u64 && len <= i64::MAX as u64 {
        // upstream: syscall.c do_punch_hole() - PUNCH_HOLE | KEEP_SIZE first.
        let punch_flags = FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE;
        match fallocate(&*file, punch_flags, pos, len) {
            Ok(()) => return Ok(()),
            Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {}
            Err(_) => {}
        }
        // ZERO_RANGE zeroes without allocation on filesystems that support it.
        match fallocate(&*file, FallocateFlags::ZERO_RANGE, pos, len) {
            Ok(()) => return Ok(()),
            Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {}
            Err(_) => {}
        }
    }

    write_zeros_fallback(file, pos, len)
}

/// Non-Linux platforms lack `fallocate(PUNCH_HOLE)`; overwrite the region with
/// explicit zeros so stale basis data does not survive an in-place update.
///
/// # Errors
///
/// Returns an error if seeking or writing the zeros fails.
#[cfg(not(target_os = "linux"))]
pub fn punch_hole(file: &mut File, pos: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    write_zeros_fallback(file, pos, len)
}

/// Writes `len` zero bytes starting at absolute offset `pos`.
///
/// Final, universal fallback: unlike hole punching this allocates disk space,
/// but it guarantees the region reads back as zeros (correctness over space).
fn write_zeros_fallback(file: &mut File, pos: u64, mut len: u64) -> io::Result<()> {
    file.seek(SeekFrom::Start(pos))?;
    let zeros = [0u8; ZERO_WRITE_BUFFER_SIZE];
    while len > 0 {
        let chunk = len.min(ZERO_WRITE_BUFFER_SIZE as u64) as usize;
        file.write_all(&zeros[..chunk])?;
        len -= chunk as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn punch_hole_zero_len_is_noop() {
        let mut f = tempfile::tempfile().expect("tempfile");
        f.write_all(&[1u8; 4096]).expect("write");
        punch_hole(&mut f, 0, 0).expect("punch");
        f.seek(SeekFrom::Start(0)).expect("seek");
        let mut buf = [0u8; 4096];
        f.read_exact(&mut buf).expect("read");
        assert!(
            buf.iter().all(|&b| b == 1),
            "zero-len punch must not alter data"
        );
    }

    #[test]
    fn punch_hole_reads_back_zeros() {
        let mut f = tempfile::tempfile().expect("tempfile");
        f.write_all(&[0xABu8; 8192]).expect("write");
        punch_hole(&mut f, 2048, 4096).expect("punch");
        f.seek(SeekFrom::Start(0)).expect("seek");
        let mut buf = [0u8; 8192];
        f.read_exact(&mut buf).expect("read");
        assert!(buf[..2048].iter().all(|&b| b == 0xAB), "prefix intact");
        assert!(
            buf[2048..6144].iter().all(|&b| b == 0),
            "punched region reads zero"
        );
        assert!(buf[6144..].iter().all(|&b| b == 0xAB), "suffix intact");
    }
}
