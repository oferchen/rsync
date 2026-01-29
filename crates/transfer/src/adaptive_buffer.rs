//! Adaptive buffer sizing based on file size.
//!
//! This module provides buffer size selection optimized for file transfer performance:
//! - Small files (< 64KB): Use small buffers to avoid wasted memory
//! - Medium files (64KB - 1MB): Use medium buffers for balanced performance
//! - Large files (> 1MB): Use large buffers to maximize throughput
//!
//! # Rationale
//!
//! Allocating large buffers for small files wastes memory and can cause cache pressure.
//! Small buffers for large files cause excessive syscall overhead. This module provides
//! right-sized buffers based on file characteristics.
//!
//! # Example
//!
//! ```
//! use transfer::adaptive_buffer::{adaptive_buffer_size, AdaptiveTokenBuffer};
//!
//! // Get buffer size for a specific file
//! let size = adaptive_buffer_size(1024 * 1024); // 1MB file
//! assert_eq!(size, 256 * 1024); // Large buffer
//!
//! // Create an adaptive token buffer
//! let mut buffer = AdaptiveTokenBuffer::for_file_size(500 * 1024);
//! buffer.resize_for(1024);
//! ```

use crate::constants::CHUNK_SIZE;

// ============================================================================
// Size Thresholds and Buffer Sizes
// ============================================================================

/// Threshold for small files (64 KB).
/// Files smaller than this use minimal buffers.
pub const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;

/// Threshold for medium files (1 MB).
/// Files smaller than this but >= SMALL_FILE_THRESHOLD use medium buffers.
pub const MEDIUM_FILE_THRESHOLD: u64 = 1024 * 1024;

/// Buffer size for small files (4 KB).
/// Minimizes memory usage for tiny files where throughput isn't critical.
pub const SMALL_BUFFER_SIZE: usize = 4 * 1024;

/// Buffer size for medium files (64 KB).
/// Balances memory usage with reasonable throughput.
pub const MEDIUM_BUFFER_SIZE: usize = 64 * 1024;

/// Buffer size for large files (256 KB).
/// Maximizes throughput for large file transfers.
pub const LARGE_BUFFER_SIZE: usize = 256 * 1024;

// ============================================================================
// Buffer Size Selection
// ============================================================================

/// Returns the optimal buffer size for a file of the given size.
///
/// # Arguments
///
/// * `file_size` - The size of the file being transferred
///
/// # Returns
///
/// The recommended buffer size in bytes:
/// - Small files (< 64KB): 4KB buffer
/// - Medium files (64KB - 1MB): 64KB buffer
/// - Large files (> 1MB): 256KB buffer
///
/// # Example
///
/// ```
/// use transfer::adaptive_buffer::adaptive_buffer_size;
///
/// assert_eq!(adaptive_buffer_size(1024), 4 * 1024);       // Small file
/// assert_eq!(adaptive_buffer_size(100 * 1024), 64 * 1024); // Medium file
/// assert_eq!(adaptive_buffer_size(10 * 1024 * 1024), 256 * 1024); // Large file
/// ```
#[inline]
#[must_use]
pub const fn adaptive_buffer_size(file_size: u64) -> usize {
    if file_size < SMALL_FILE_THRESHOLD {
        SMALL_BUFFER_SIZE
    } else if file_size < MEDIUM_FILE_THRESHOLD {
        MEDIUM_BUFFER_SIZE
    } else {
        LARGE_BUFFER_SIZE
    }
}

/// Returns the optimal BufWriter capacity for a file of the given size.
///
/// This is specifically for `std::io::BufWriter` capacity, which may
/// use the same or different sizing strategy as token buffers.
///
/// # Arguments
///
/// * `file_size` - The size of the file being written
///
/// # Returns
///
/// The recommended BufWriter capacity in bytes.
#[inline]
#[must_use]
pub const fn adaptive_writer_capacity(file_size: u64) -> usize {
    adaptive_buffer_size(file_size)
}

/// Returns the optimal TokenBuffer initial capacity for a file of the given size.
///
/// TokenBuffer capacity determines the initial allocation for reading literal
/// delta tokens. The buffer grows as needed, but starting with an appropriate
/// size avoids reallocation for most files.
///
/// # Arguments
///
/// * `file_size` - The size of the file being transferred
///
/// # Returns
///
/// The recommended initial TokenBuffer capacity.
#[inline]
#[must_use]
pub const fn adaptive_token_capacity(file_size: u64) -> usize {
    // For token buffers, we use slightly smaller initial sizes since tokens
    // are typically smaller than the file itself and the buffer can grow.
    if file_size < SMALL_FILE_THRESHOLD {
        SMALL_BUFFER_SIZE
    } else if file_size < MEDIUM_FILE_THRESHOLD {
        // Medium files: use CHUNK_SIZE (32KB) as a reasonable middle ground
        CHUNK_SIZE
    } else {
        // Large files: use medium buffer size since tokens rarely exceed 64KB
        MEDIUM_BUFFER_SIZE
    }
}

// ============================================================================
// Adaptive Token Buffer
// ============================================================================

/// A reusable buffer for delta token literal data with adaptive initial sizing.
///
/// This wraps the token buffer pattern with file-size-aware initial capacity
/// to optimize memory usage across different file sizes.
///
/// # Performance
///
/// - Small files: Starts with 4KB capacity, avoiding waste
/// - Medium files: Starts with 32KB (CHUNK_SIZE), good for most tokens
/// - Large files: Starts with 64KB, reducing reallocation for large tokens
///
/// The buffer grows as needed but never shrinks, making it efficient for
/// reuse across multiple tokens in a single file transfer.
#[derive(Debug)]
pub struct AdaptiveTokenBuffer {
    /// Internal storage that grows but never shrinks.
    data: Vec<u8>,
    /// Current logical length (may be less than capacity).
    len: usize,
}

impl Default for AdaptiveTokenBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveTokenBuffer {
    /// Creates a new empty token buffer.
    ///
    /// The buffer starts empty and grows on first use.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: Vec::new(),
            len: 0,
        }
    }

    /// Creates a token buffer with capacity optimized for the given file size.
    ///
    /// This is the recommended way to create a buffer when you know the file size.
    ///
    /// # Arguments
    ///
    /// * `file_size` - The size of the file being transferred
    #[must_use]
    pub fn for_file_size(file_size: u64) -> Self {
        let capacity = adaptive_token_capacity(file_size);
        Self {
            data: Vec::with_capacity(capacity),
            len: 0,
        }
    }

    /// Creates a token buffer with the specified initial capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
            len: 0,
        }
    }

    /// Ensures the buffer can hold at least `size` bytes.
    ///
    /// If the current capacity is insufficient, the buffer grows.
    /// The buffer never shrinks, so subsequent smaller requests are free.
    pub fn resize_for(&mut self, size: usize) {
        if self.data.len() < size {
            self.data.resize(size, 0);
        }
        self.len = size;
    }

    /// Returns the current logical length.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the logical length is zero.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the allocated capacity.
    #[must_use]
    #[inline]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Resets the logical length to zero without deallocating.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns a slice of the buffer's logical contents.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Returns a mutable slice of the buffer's allocated storage.
    #[must_use]
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data[..self.len]
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_size_thresholds() {
        // Small files (< 64KB)
        assert_eq!(adaptive_buffer_size(0), SMALL_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(1), SMALL_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(1024), SMALL_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(SMALL_FILE_THRESHOLD - 1), SMALL_BUFFER_SIZE);

        // Medium files (64KB - 1MB)
        assert_eq!(adaptive_buffer_size(SMALL_FILE_THRESHOLD), MEDIUM_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(100 * 1024), MEDIUM_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(500 * 1024), MEDIUM_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(MEDIUM_FILE_THRESHOLD - 1), MEDIUM_BUFFER_SIZE);

        // Large files (> 1MB)
        assert_eq!(adaptive_buffer_size(MEDIUM_FILE_THRESHOLD), LARGE_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(10 * 1024 * 1024), LARGE_BUFFER_SIZE);
        assert_eq!(adaptive_buffer_size(1024 * 1024 * 1024), LARGE_BUFFER_SIZE);
    }

    #[test]
    fn writer_capacity_matches_buffer_size() {
        for size in [0, 1024, 65536, 500_000, 2_000_000] {
            assert_eq!(
                adaptive_writer_capacity(size),
                adaptive_buffer_size(size),
                "writer capacity should match buffer size for file_size={size}"
            );
        }
    }

    #[test]
    fn token_capacity_for_small_files() {
        assert_eq!(adaptive_token_capacity(0), SMALL_BUFFER_SIZE);
        assert_eq!(adaptive_token_capacity(1024), SMALL_BUFFER_SIZE);
    }

    #[test]
    fn token_capacity_for_medium_files() {
        assert_eq!(adaptive_token_capacity(100 * 1024), CHUNK_SIZE);
    }

    #[test]
    fn token_capacity_for_large_files() {
        assert_eq!(adaptive_token_capacity(10 * 1024 * 1024), MEDIUM_BUFFER_SIZE);
    }

    #[test]
    fn adaptive_token_buffer_new() {
        let buffer = AdaptiveTokenBuffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn adaptive_token_buffer_for_small_file() {
        let buffer = AdaptiveTokenBuffer::for_file_size(1024);
        assert!(buffer.is_empty());
        assert!(buffer.capacity() >= SMALL_BUFFER_SIZE);
    }

    #[test]
    fn adaptive_token_buffer_for_medium_file() {
        let buffer = AdaptiveTokenBuffer::for_file_size(100 * 1024);
        assert!(buffer.is_empty());
        assert!(buffer.capacity() >= CHUNK_SIZE);
    }

    #[test]
    fn adaptive_token_buffer_for_large_file() {
        let buffer = AdaptiveTokenBuffer::for_file_size(10 * 1024 * 1024);
        assert!(buffer.is_empty());
        assert!(buffer.capacity() >= MEDIUM_BUFFER_SIZE);
    }

    #[test]
    fn adaptive_token_buffer_resize_and_reuse() {
        let mut buffer = AdaptiveTokenBuffer::for_file_size(1024);

        // First resize
        buffer.resize_for(100);
        assert_eq!(buffer.len(), 100);

        // Smaller resize (no realloc)
        let cap = buffer.capacity();
        buffer.resize_for(50);
        assert_eq!(buffer.len(), 50);
        assert_eq!(buffer.capacity(), cap);

        // Larger resize (may realloc)
        buffer.resize_for(1000);
        assert_eq!(buffer.len(), 1000);
        assert!(buffer.capacity() >= 1000);
    }

    #[test]
    fn adaptive_token_buffer_slice_access() {
        let mut buffer = AdaptiveTokenBuffer::for_file_size(1024);
        buffer.resize_for(5);
        buffer.as_mut_slice().copy_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(buffer.as_slice(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn adaptive_token_buffer_clear() {
        let mut buffer = AdaptiveTokenBuffer::for_file_size(1024);
        buffer.resize_for(100);
        let cap = buffer.capacity();
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.capacity(), cap);
    }

    // ========================================================================
    // Additional comprehensive tests
    // ========================================================================

    #[test]
    fn boundary_conditions_at_thresholds() {
        // Exact boundary at 64KB threshold
        assert_eq!(
            adaptive_buffer_size(SMALL_FILE_THRESHOLD),
            MEDIUM_BUFFER_SIZE,
            "exactly at 64KB should use medium buffer"
        );
        assert_eq!(
            adaptive_buffer_size(SMALL_FILE_THRESHOLD - 1),
            SMALL_BUFFER_SIZE,
            "one byte below 64KB should use small buffer"
        );

        // Exact boundary at 1MB threshold
        assert_eq!(
            adaptive_buffer_size(MEDIUM_FILE_THRESHOLD),
            LARGE_BUFFER_SIZE,
            "exactly at 1MB should use large buffer"
        );
        assert_eq!(
            adaptive_buffer_size(MEDIUM_FILE_THRESHOLD - 1),
            MEDIUM_BUFFER_SIZE,
            "one byte below 1MB should use medium buffer"
        );
    }

    #[test]
    fn zero_sized_files() {
        // Zero-sized files should still get a reasonable buffer
        assert_eq!(adaptive_buffer_size(0), SMALL_BUFFER_SIZE);
        assert_eq!(adaptive_writer_capacity(0), SMALL_BUFFER_SIZE);
        assert_eq!(adaptive_token_capacity(0), SMALL_BUFFER_SIZE);

        // AdaptiveTokenBuffer should handle zero-sized files
        let buffer = AdaptiveTokenBuffer::for_file_size(0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
        assert!(buffer.capacity() >= SMALL_BUFFER_SIZE);
    }

    #[test]
    fn adaptive_token_buffer_with_capacity() {
        let buffer = AdaptiveTokenBuffer::with_capacity(8192);
        assert!(buffer.is_empty());
        assert!(buffer.capacity() >= 8192);
    }

    #[test]
    fn adaptive_token_buffer_default() {
        let buffer = AdaptiveTokenBuffer::default();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn adaptive_token_buffer_resize_zero() {
        let mut buffer = AdaptiveTokenBuffer::for_file_size(1024);
        buffer.resize_for(100);
        buffer.resize_for(0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn adaptive_token_buffer_multiple_resizes() {
        let mut buffer = AdaptiveTokenBuffer::new();

        // Growth sequence
        buffer.resize_for(10);
        assert_eq!(buffer.len(), 10);

        buffer.resize_for(100);
        assert_eq!(buffer.len(), 100);

        buffer.resize_for(1000);
        assert_eq!(buffer.len(), 1000);

        // Capacity should never decrease
        let cap = buffer.capacity();
        buffer.resize_for(50);
        assert_eq!(buffer.len(), 50);
        assert!(buffer.capacity() >= cap);
    }

    #[test]
    fn token_capacity_at_boundaries() {
        // At small threshold boundary
        assert_eq!(
            adaptive_token_capacity(SMALL_FILE_THRESHOLD - 1),
            SMALL_BUFFER_SIZE
        );
        assert_eq!(
            adaptive_token_capacity(SMALL_FILE_THRESHOLD),
            CHUNK_SIZE
        );

        // At medium threshold boundary
        assert_eq!(
            adaptive_token_capacity(MEDIUM_FILE_THRESHOLD - 1),
            CHUNK_SIZE
        );
        assert_eq!(
            adaptive_token_capacity(MEDIUM_FILE_THRESHOLD),
            MEDIUM_BUFFER_SIZE
        );
    }

    #[test]
    fn buffer_size_constants_are_sensible() {
        // Verify the constants are in ascending order
        assert!(SMALL_BUFFER_SIZE < MEDIUM_BUFFER_SIZE);
        assert!(MEDIUM_BUFFER_SIZE < LARGE_BUFFER_SIZE);

        // Verify thresholds are in ascending order
        assert!(SMALL_FILE_THRESHOLD < MEDIUM_FILE_THRESHOLD);

        // Verify buffer sizes are reasonable
        assert_eq!(SMALL_BUFFER_SIZE, 4 * 1024);
        assert_eq!(MEDIUM_BUFFER_SIZE, 64 * 1024);
        assert_eq!(LARGE_BUFFER_SIZE, 256 * 1024);
    }

    #[test]
    fn very_large_file_sizes() {
        // Test with extremely large file sizes
        let huge_file = 10u64 * 1024 * 1024 * 1024; // 10 GB
        assert_eq!(adaptive_buffer_size(huge_file), LARGE_BUFFER_SIZE);
        assert_eq!(adaptive_writer_capacity(huge_file), LARGE_BUFFER_SIZE);
        assert_eq!(adaptive_token_capacity(huge_file), MEDIUM_BUFFER_SIZE);

        // Even for huge files, token capacity is capped at medium
        let massive_file = u64::MAX;
        assert_eq!(adaptive_token_capacity(massive_file), MEDIUM_BUFFER_SIZE);
    }

    #[test]
    fn adaptive_token_buffer_no_allocation_on_new() {
        let buffer = AdaptiveTokenBuffer::new();
        // New buffers should not allocate
        assert_eq!(buffer.capacity(), 0);
    }

    #[test]
    fn adaptive_token_buffer_resize_idempotent() {
        let mut buffer = AdaptiveTokenBuffer::for_file_size(1024);

        // Resize to same size multiple times
        buffer.resize_for(100);
        let cap1 = buffer.capacity();

        buffer.resize_for(100);
        let cap2 = buffer.capacity();

        buffer.resize_for(100);
        let cap3 = buffer.capacity();

        assert_eq!(cap1, cap2);
        assert_eq!(cap2, cap3);
        assert_eq!(buffer.len(), 100);
    }

    #[test]
    fn writer_capacity_exact_values() {
        // Verify exact values for common file sizes
        assert_eq!(adaptive_writer_capacity(1024), 4 * 1024);
        assert_eq!(adaptive_writer_capacity(64 * 1024), 64 * 1024);
        assert_eq!(adaptive_writer_capacity(1024 * 1024), 256 * 1024);
    }

    #[test]
    fn adaptive_token_buffer_capacity_growth() {
        let mut buffer = AdaptiveTokenBuffer::with_capacity(10);

        // Resize within initial capacity
        buffer.resize_for(5);
        assert_eq!(buffer.len(), 5);
        assert!(buffer.capacity() >= 10);

        // Resize beyond initial capacity
        buffer.resize_for(100);
        assert_eq!(buffer.len(), 100);
        assert!(buffer.capacity() >= 100);

        // Resize way beyond
        buffer.resize_for(10_000);
        assert_eq!(buffer.len(), 10_000);
        assert!(buffer.capacity() >= 10_000);
    }

    #[test]
    fn adaptive_token_buffer_slice_reflects_length() {
        let mut buffer = AdaptiveTokenBuffer::for_file_size(1024);

        buffer.resize_for(10);
        assert_eq!(buffer.as_slice().len(), 10);

        buffer.resize_for(50);
        assert_eq!(buffer.as_slice().len(), 50);

        buffer.resize_for(5);
        assert_eq!(buffer.as_slice().len(), 5);
    }
}
