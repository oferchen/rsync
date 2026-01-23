//! Buffer size constants mirroring upstream rsync.
//!
//! These constants match the definitions in upstream rsync's `rsync.h` to ensure
//! compatible behavior and optimal performance characteristics.
//!
//! # Upstream Reference
//!
//! See `rsync.h` in upstream rsync 3.4.1 for the original definitions.

/// Chunk size for checksum computation and sparse file processing.
///
/// Used for iterating over file data in fixed-size chunks during:
/// - Strong checksum computation
/// - Sparse file zero-run detection
/// - Delta token processing
///
/// Matches upstream `CHUNK_SIZE` (32 * 1024).
pub const CHUNK_SIZE: usize = 32 * 1024;

/// Maximum size of the memory-mapped file window.
///
/// When reading basis files for delta application, data is cached in a
/// sliding window of this size to avoid repeated seek/read syscalls.
///
/// Matches upstream `MAX_MAP_SIZE` (256 * 1024).
pub const MAX_MAP_SIZE: usize = 256 * 1024;

/// Default I/O buffer size for multiplexed protocol streams.
///
/// Used for buffering data in the multiplex reader/writer layers
/// before flushing to the underlying transport.
///
/// Matches upstream `IO_BUFFER_SIZE` (32 * 1024).
pub const IO_BUFFER_SIZE: usize = 32 * 1024;

/// Alignment boundary for file read operations.
///
/// Read positions are aligned to this boundary to improve I/O efficiency
/// and cache utilization.
///
/// Matches upstream `ALIGN_BOUNDARY` (1024).
pub const ALIGN_BOUNDARY: usize = 1024;

/// Default rsync block size for delta computation.
///
/// This is the minimum block size used when computing file signatures.
/// Actual block size may be larger for big files.
///
/// Matches upstream `BLOCK_SIZE` (700).
pub const BLOCK_SIZE: usize = 700;

/// Maximum block size for protocol version 30+.
///
/// Matches upstream `MAX_BLOCK_SIZE` (1 << 17 = 128KB).
pub const MAX_BLOCK_SIZE: usize = 1 << 17;

/// Maximum block size for protocol versions before 30.
///
/// Matches upstream `OLD_MAX_BLOCK_SIZE` (1 << 29 = 512MB).
pub const OLD_MAX_BLOCK_SIZE: usize = 1 << 29;

/// Aligns an offset down to the nearest alignment boundary.
#[inline]
#[must_use]
pub const fn align_down(offset: u64) -> u64 {
    offset & !(ALIGN_BOUNDARY as u64 - 1)
}

/// Aligns a length up to the nearest alignment boundary.
#[inline]
#[must_use]
pub const fn align_up(len: usize) -> usize {
    ((len.saturating_sub(1)) | (ALIGN_BOUNDARY - 1)) + 1
}

/// Returns how far past the alignment boundary an offset is.
#[inline]
#[must_use]
pub const fn aligned_overshoot(offset: u64) -> usize {
    (offset & (ALIGN_BOUNDARY as u64 - 1)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_upstream() {
        assert_eq!(CHUNK_SIZE, 32 * 1024);
        assert_eq!(MAX_MAP_SIZE, 256 * 1024);
        assert_eq!(IO_BUFFER_SIZE, 32 * 1024);
        assert_eq!(ALIGN_BOUNDARY, 1024);
        assert_eq!(BLOCK_SIZE, 700);
        assert_eq!(MAX_BLOCK_SIZE, 131072);
    }

    #[test]
    fn align_down_at_boundary() {
        assert_eq!(align_down(1024), 1024);
        assert_eq!(align_down(2048), 2048);
    }

    #[test]
    fn align_down_between_boundaries() {
        assert_eq!(align_down(1025), 1024);
        assert_eq!(align_down(2000), 1024);
        assert_eq!(align_down(2047), 1024);
    }

    #[test]
    fn align_down_zero() {
        assert_eq!(align_down(0), 0);
    }

    #[test]
    fn align_up_at_boundary() {
        assert_eq!(align_up(1024), 1024);
        assert_eq!(align_up(2048), 2048);
    }

    #[test]
    fn align_up_between_boundaries() {
        assert_eq!(align_up(1), 1024);
        assert_eq!(align_up(1025), 2048);
        assert_eq!(align_up(2000), 2048);
    }

    #[test]
    fn aligned_overshoot_at_boundary() {
        assert_eq!(aligned_overshoot(1024), 0);
        assert_eq!(aligned_overshoot(2048), 0);
    }

    #[test]
    fn aligned_overshoot_between_boundaries() {
        assert_eq!(aligned_overshoot(1025), 1);
        assert_eq!(aligned_overshoot(2000), 976);
    }
}
