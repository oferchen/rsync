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

// ============================================================================
// Optimized Zero Detection
// ============================================================================
//
// These functions use 128-bit integer comparison for fast zero detection,
// processing 16 bytes at a time. On x86-64, u128 operations are optimized
// to use SSE/AVX registers, providing SIMD-like performance.

/// Counts the number of leading zero bytes in a slice.
///
/// Uses 16-byte chunks with u128 comparison for fast detection,
/// falling back to byte-by-byte scanning for the remainder.
///
/// # Performance
///
/// Processes 16 bytes per iteration using native u128 comparison,
/// which the compiler optimizes to SIMD instructions on x86-64.
#[inline]
pub fn leading_zero_count(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut iter = bytes.chunks_exact(16);

    for chunk in &mut iter {
        // SAFETY: chunks_exact(16) guarantees exactly 16-byte slices
        let chunk: &[u8; 16] = chunk.try_into().expect("chunks_exact guarantees 16 bytes");
        if u128::from_ne_bytes(*chunk) == 0 {
            offset += 16;
        } else {
            // Found non-zero in this chunk - scan for exact position
            let position = chunk.iter().position(|&b| b != 0).unwrap_or(16);
            return offset + position;
        }
    }

    // Handle remainder (< 16 bytes)
    offset + iter.remainder().iter().take_while(|&&b| b == 0).count()
}

/// Counts the number of trailing zero bytes in a slice.
///
/// Uses 16-byte chunks with u128 comparison for fast detection,
/// scanning from the end of the slice.
///
/// # Performance
///
/// Processes 16 bytes per iteration using native u128 comparison,
/// which the compiler optimizes to SIMD instructions on x86-64.
#[inline]
pub fn trailing_zero_count(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut iter = bytes.rchunks_exact(16);

    for chunk in &mut iter {
        // SAFETY: rchunks_exact(16) guarantees exactly 16-byte slices
        let chunk: &[u8; 16] = chunk.try_into().expect("rchunks_exact guarantees 16 bytes");
        if u128::from_ne_bytes(*chunk) == 0 {
            offset += 16;
        } else {
            // Found non-zero in this chunk - scan for exact position
            let trailing = chunk.iter().rev().take_while(|&&b| b == 0).count();
            return offset + trailing;
        }
    }

    // Handle remainder (< 16 bytes)
    offset
        + iter
            .remainder()
            .iter()
            .rev()
            .take_while(|&&b| b == 0)
            .count()
}

/// Checks if a buffer contains only zeros.
///
/// Optimized for large buffers using 16-byte u128 comparisons.
#[inline]
pub fn is_all_zeros(bytes: &[u8]) -> bool {
    leading_zero_count(bytes) == bytes.len()
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

    // Zero detection tests

    #[test]
    fn leading_zero_count_empty() {
        assert_eq!(leading_zero_count(&[]), 0);
    }

    #[test]
    fn leading_zero_count_all_zeros() {
        assert_eq!(leading_zero_count(&[0; 1]), 1);
        assert_eq!(leading_zero_count(&[0; 15]), 15);
        assert_eq!(leading_zero_count(&[0; 16]), 16);
        assert_eq!(leading_zero_count(&[0; 17]), 17);
        assert_eq!(leading_zero_count(&[0; 32]), 32);
        assert_eq!(leading_zero_count(&[0; 100]), 100);
    }

    #[test]
    fn leading_zero_count_no_zeros() {
        assert_eq!(leading_zero_count(&[1]), 0);
        assert_eq!(leading_zero_count(&[1, 2, 3]), 0);
    }

    #[test]
    fn leading_zero_count_mixed() {
        assert_eq!(leading_zero_count(&[0, 0, 1, 0]), 2);
        assert_eq!(leading_zero_count(&[0, 0, 0, 0, 0, 1]), 5);
        // Test at 16-byte boundary
        let mut data = vec![0u8; 20];
        data[16] = 1;
        assert_eq!(leading_zero_count(&data), 16);
    }

    #[test]
    fn trailing_zero_count_empty() {
        assert_eq!(trailing_zero_count(&[]), 0);
    }

    #[test]
    fn trailing_zero_count_all_zeros() {
        assert_eq!(trailing_zero_count(&[0; 1]), 1);
        assert_eq!(trailing_zero_count(&[0; 15]), 15);
        assert_eq!(trailing_zero_count(&[0; 16]), 16);
        assert_eq!(trailing_zero_count(&[0; 17]), 17);
        assert_eq!(trailing_zero_count(&[0; 32]), 32);
    }

    #[test]
    fn trailing_zero_count_no_zeros() {
        assert_eq!(trailing_zero_count(&[1]), 0);
        assert_eq!(trailing_zero_count(&[1, 2, 3]), 0);
    }

    #[test]
    fn trailing_zero_count_mixed() {
        assert_eq!(trailing_zero_count(&[1, 0, 0]), 2);
        assert_eq!(trailing_zero_count(&[1, 0, 0, 0, 0, 0]), 5);
        // Test at 16-byte boundary
        let mut data = vec![0u8; 20];
        data[3] = 1;
        assert_eq!(trailing_zero_count(&data), 16);
    }

    #[test]
    fn is_all_zeros_works() {
        assert!(is_all_zeros(&[]));
        assert!(is_all_zeros(&[0]));
        assert!(is_all_zeros(&[0; 100]));
        assert!(!is_all_zeros(&[1]));
        assert!(!is_all_zeros(&[0, 0, 1]));
    }
}
