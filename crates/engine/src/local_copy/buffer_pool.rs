//! Thread-safe buffer pool for reusing I/O buffers.
//!
//! This module provides a [`BufferPool`] that reduces allocation overhead during
//! file copy operations by reusing fixed-size buffers. Buffers are automatically
//! returned to the pool when the [`BufferGuard`] is dropped.
//!
//! # Adaptive Buffer Sizing
//!
//! The `adaptive_buffer_size` function selects an appropriate I/O buffer size
//! based on the file being transferred. Small files use smaller buffers to reduce
//! memory overhead, while large files use larger buffers for better throughput.
//! Use [`BufferPool::acquire_adaptive_from`] to acquire a buffer sized to match
//! the file being transferred.
//!
//! # Design
//!
//! The pool uses a simple stack-based approach: buffers are pushed when released
//! and popped when acquired. This provides good cache locality as recently-used
//! buffers are reused first.
//!
//! # Thread Safety
//!
//! The pool uses [`std::sync::Mutex`] for thread-safe access. The lock is held
//! only briefly during acquire/release operations, minimizing contention.
//!
//! # Ownership Model
//!
//! The pool is wrapped in [`Arc`](std::sync::Arc) to allow [`BufferGuard`] to hold an owned
//! reference, avoiding borrow checker issues when the pool is part of a larger
//! context struct.
//!
//! # Example
//!
//! ```ignore
//! use engine::local_copy::buffer_pool::BufferPool;
//! use std::sync::Arc;
//!
//! let pool = Arc::new(BufferPool::new(4));
//! let buffer = BufferPool::acquire_from(Arc::clone(&pool));
//! // Use buffer for I/O...
//! // Buffer automatically returned to pool on drop
//! ```

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use super::COPY_BUFFER_SIZE;

/// Buffer size for files smaller than 64 KB (8 KB).
pub const ADAPTIVE_BUFFER_TINY: usize = super::ADAPTIVE_BUFFER_TINY;
/// Buffer size for files in the 64 KB .. 1 MB range (32 KB).
pub const ADAPTIVE_BUFFER_SMALL: usize = super::ADAPTIVE_BUFFER_SMALL;
/// Buffer size for files in the 1 MB .. 64 MB range (128 KB).
pub const ADAPTIVE_BUFFER_MEDIUM: usize = super::ADAPTIVE_BUFFER_MEDIUM;
/// Buffer size for files 64 MB and larger (512 KB).
pub const ADAPTIVE_BUFFER_LARGE: usize = super::ADAPTIVE_BUFFER_LARGE;

/// Selects an I/O buffer size appropriate for the given file size.
///
/// The returned size balances memory consumption against throughput:
///
/// | File size          | Buffer size |
/// |--------------------|-------------|
/// | < 64 KB            | 8 KB        |
/// | 64 KB .. < 1 MB    | 32 KB       |
/// | 1 MB .. < 64 MB    | 128 KB      |
/// | >= 64 MB           | 512 KB      |
///
/// # Examples
///
/// ```
/// use engine::local_copy::buffer_pool::adaptive_buffer_size;
///
/// assert_eq!(adaptive_buffer_size(1_000), 8 * 1024);
/// assert_eq!(adaptive_buffer_size(500_000), 32 * 1024);
/// assert_eq!(adaptive_buffer_size(10_000_000), 128 * 1024);
/// assert_eq!(adaptive_buffer_size(100_000_000), 512 * 1024);
/// ```
#[must_use]
pub const fn adaptive_buffer_size(file_size: u64) -> usize {
    super::adaptive_buffer_size(file_size)
}

/// A thread-safe pool of reusable I/O buffers.
///
/// Reduces allocation overhead during file copy operations by maintaining
/// a pool of fixed-size buffers that can be reused across operations.
///
/// # Capacity
///
/// The pool has a maximum capacity to prevent unbounded memory growth.
/// When the pool is full, returning a buffer simply drops it.
/// When the pool is empty, acquiring creates a new buffer.
#[derive(Debug)]
pub struct BufferPool {
    /// Stack of available buffers, protected by mutex.
    buffers: Mutex<Vec<Vec<u8>>>,
    /// Maximum number of buffers to retain in the pool.
    max_buffers: usize,
    /// Size of each buffer in bytes.
    buffer_size: usize,
}

impl BufferPool {
    /// Creates a new buffer pool with the specified maximum capacity.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain. Excess buffers
    ///   are dropped when returned to the pool.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pool = BufferPool::new(num_cpus::get());
    /// ```
    #[must_use]
    pub fn new(max_buffers: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            max_buffers,
            buffer_size: COPY_BUFFER_SIZE,
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            max_buffers,
            buffer_size,
        }
    }

    /// Acquires a buffer from the pool using an Arc reference.
    ///
    /// This is the preferred method when the pool is part of a larger struct
    /// that needs to be mutably borrowed while the buffer is in use.
    ///
    /// Returns a pooled buffer if available, otherwise allocates a new one.
    /// The returned [`BufferGuard`] automatically returns the buffer to the
    /// pool when dropped.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn acquire_from(pool: Arc<Self>) -> BufferGuard {
        let buffer = {
            let mut buffers = pool.buffers.lock().expect("buffer pool mutex poisoned");
            buffers.pop()
        };

        let buffer = buffer.unwrap_or_else(|| vec![0u8; pool.buffer_size]);

        BufferGuard {
            buffer: Some(buffer),
            pool,
        }
    }

    /// Acquires a buffer sized adaptively for the given file size.
    ///
    /// Uses [`adaptive_buffer_size`] to choose the buffer length. If the
    /// adaptive size matches the pool's default buffer size, a pooled buffer
    /// is returned when available. Otherwise a fresh buffer of the adaptive
    /// size is allocated (it will still be returned to the pool on drop,
    /// where its length is restored to the pool's default).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn acquire_adaptive_from(pool: Arc<Self>, file_size: u64) -> BufferGuard {
        let desired = adaptive_buffer_size(file_size);

        if desired == pool.buffer_size {
            // Fast path: adaptive size matches pool default -- reuse pooled buffers.
            return Self::acquire_from(pool);
        }

        // Slow path: non-standard size -- allocate a fresh buffer.
        // On drop the guard will pass it through `return_buffer` which
        // resizes it to the pool default before returning it.
        BufferGuard {
            buffer: Some(vec![0u8; desired]),
            pool,
        }
    }

    /// Acquires a buffer from the pool (borrows self).
    ///
    /// **Note:** This method returns a guard with a lifetime tied to `self`.
    /// Use [`acquire_from`](Self::acquire_from) when the pool is part of a
    /// larger context that needs to be mutably borrowed.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn acquire(&self) -> BorrowedBufferGuard<'_> {
        let buffer = {
            let mut pool = self.buffers.lock().expect("buffer pool mutex poisoned");
            pool.pop()
        };

        let buffer = buffer.unwrap_or_else(|| vec![0u8; self.buffer_size]);

        BorrowedBufferGuard {
            buffer: Some(buffer),
            pool: self,
        }
    }

    /// Returns a buffer to the pool.
    ///
    /// The buffer's logical length is restored to the pool's default size
    /// without zeroing the contents. This is safe because every consumer
    /// overwrites the buffer via [`Read::read`] before consuming data (see
    /// `transfer.rs` and `parallel_checksum.rs`).
    ///
    /// If the pool is at capacity, the buffer is dropped instead.
    fn return_buffer(&self, mut buffer: Vec<u8>) {
        if buffer.capacity() < self.buffer_size {
            // Small adaptive buffer — replace with fresh allocation at pool size.
            buffer = Vec::with_capacity(self.buffer_size);
        }
        // SAFETY: capacity >= self.buffer_size is guaranteed by the branch
        // above (fresh allocation) or by the original allocation (same-size
        // or larger adaptive buffer). The stale contents will be fully
        // overwritten by the next Read::read() before being consumed.
        // This avoids the expensive `resize(size, 0)` memset that was the
        // #1 CPU hotspot (26% of runtime per flamegraph profiling).
        unsafe { buffer.set_len(self.buffer_size) };

        let mut pool = self.buffers.lock().expect("buffer pool mutex poisoned");
        if pool.len() < self.max_buffers {
            pool.push(buffer);
        }
    }

    /// Returns the number of buffers currently in the pool.
    ///
    /// This is primarily useful for testing and monitoring.
    #[must_use]
    pub fn available(&self) -> usize {
        self.buffers
            .lock()
            .expect("buffer pool mutex poisoned")
            .len()
    }

    /// Returns the maximum number of buffers the pool will retain.
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.max_buffers
    }

    /// Returns the size of each buffer in bytes.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}

impl Default for BufferPool {
    /// Creates a buffer pool with capacity based on available parallelism.
    fn default() -> Self {
        let max_buffers = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);
        Self::new(max_buffers)
    }
}

/// RAII guard that returns a buffer to the pool on drop (owned version).
///
/// This guard holds an [`Arc`] to the pool, allowing it to be used when
/// the pool is part of a larger context that needs to be mutably borrowed.
///
/// Provides transparent access to the underlying buffer via [`Deref`] and
/// [`DerefMut`], allowing it to be used wherever `&[u8]` or `&mut [u8]`
/// is expected.
#[derive(Debug)]
pub struct BufferGuard {
    /// The buffer, wrapped in Option for take-on-drop pattern.
    buffer: Option<Vec<u8>>,
    /// Arc reference to the pool for returning the buffer.
    pool: Arc<BufferPool>,
}

impl Deref for BufferGuard {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buffer.as_ref().expect("buffer already taken")
    }
}

impl DerefMut for BufferGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

impl BufferGuard {
    /// Returns the length of the buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map(Vec::len).unwrap_or(0)
    }

    /// Returns true if the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the buffer as a mutable slice.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

/// RAII guard that returns a buffer to the pool on drop (borrowed version).
///
/// This guard borrows the pool, suitable for simple use cases where the pool
/// lifetime is clear.
#[derive(Debug)]
pub struct BorrowedBufferGuard<'a> {
    /// The buffer, wrapped in Option for take-on-drop pattern.
    buffer: Option<Vec<u8>>,
    /// Reference to the pool for returning the buffer.
    pool: &'a BufferPool,
}

impl Deref for BorrowedBufferGuard<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buffer.as_ref().expect("buffer already taken")
    }
}

impl DerefMut for BorrowedBufferGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

impl Drop for BorrowedBufferGuard<'_> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

impl BorrowedBufferGuard<'_> {
    /// Returns the length of the buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map(Vec::len).unwrap_or(0)
    }

    /// Returns true if the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the buffer as a mutable slice.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_acquire_returns_buffer() {
        let pool = BufferPool::new(4);
        let buffer = pool.acquire();
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    }

    #[test]
    fn test_buffer_reuse() {
        let pool = BufferPool::new(4);

        // Acquire and release a buffer
        {
            let mut buffer = pool.acquire();
            buffer[0] = 42;
        }

        // Pool should have one buffer
        assert_eq!(pool.available(), 1);

        // Acquire again - should get the reused buffer with correct length
        let buffer = pool.acquire();
        assert_eq!(pool.available(), 0);
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    }

    #[test]
    fn test_pool_capacity_limit() {
        let pool = BufferPool::new(2);

        // Acquire 3 buffers
        let b1 = pool.acquire();
        let b2 = pool.acquire();
        let b3 = pool.acquire();

        // Release all
        drop(b1);
        drop(b2);
        drop(b3);

        // Only 2 should be retained
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn test_concurrent_access() {
        let pool = Arc::new(BufferPool::new(8));
        let mut handles = vec![];

        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let mut buffer = pool.acquire();
                    buffer[0] = 1;
                    // Buffer returned on drop
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // Pool should have at most max_buffers
        assert!(pool.available() <= 8);
    }

    #[test]
    fn test_buffer_guard_deref() {
        let pool = BufferPool::new(4);
        let mut buffer = pool.acquire();

        // Write through DerefMut
        buffer[0] = 100;
        buffer[1] = 200;

        // Read through Deref
        assert_eq!(buffer[0], 100);
        assert_eq!(buffer[1], 200);

        // Use as slice
        let slice: &[u8] = &buffer;
        assert_eq!(slice[0], 100);
    }

    #[test]
    fn test_buffer_guard_as_mut_slice() {
        let pool = BufferPool::new(4);
        let mut buffer = pool.acquire();

        let slice = buffer.as_mut_slice();
        slice[0] = 42;

        assert_eq!(buffer[0], 42);
    }

    #[test]
    fn test_custom_buffer_size() {
        let pool = BufferPool::with_buffer_size(4, 1024);
        let buffer = pool.acquire();
        assert_eq!(buffer.len(), 1024);
        assert_eq!(pool.buffer_size(), 1024);
    }

    #[test]
    fn test_default_pool() {
        let pool = BufferPool::default();
        assert!(pool.max_buffers() > 0);
        assert_eq!(pool.buffer_size(), COPY_BUFFER_SIZE);
    }

    #[test]
    fn test_buffer_length_restored_on_return() {
        let pool = BufferPool::new(4);

        {
            let mut buffer = pool.acquire();
            // Fill with non-zero data
            for byte in buffer.iter_mut() {
                *byte = 0xFF;
            }
        }

        // Acquire again — length should be restored (contents are stale but
        // will be overwritten by Read::read before consumption).
        let buffer = pool.acquire();
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    }

    // -----------------------------------------------------------------------
    // Adaptive buffer sizing tests
    // -----------------------------------------------------------------------

    #[test]
    fn adaptive_size_zero_byte_file() {
        assert_eq!(adaptive_buffer_size(0), ADAPTIVE_BUFFER_TINY);
    }

    #[test]
    fn adaptive_size_one_byte_file() {
        assert_eq!(adaptive_buffer_size(1), ADAPTIVE_BUFFER_TINY);
    }

    #[test]
    fn adaptive_size_tiny_file() {
        // A 1 KB file should get an 8 KB buffer.
        assert_eq!(adaptive_buffer_size(1024), ADAPTIVE_BUFFER_TINY);
    }

    #[test]
    fn adaptive_size_just_below_tiny_threshold() {
        // 64 KB - 1 byte: still in the tiny range.
        assert_eq!(adaptive_buffer_size(64 * 1024 - 1), ADAPTIVE_BUFFER_TINY);
    }

    #[test]
    fn adaptive_size_at_tiny_threshold() {
        // Exactly 64 KB: enters the small range.
        assert_eq!(adaptive_buffer_size(64 * 1024), ADAPTIVE_BUFFER_SMALL);
    }

    #[test]
    fn adaptive_size_small_file() {
        // 500 KB file should get a 32 KB buffer.
        assert_eq!(adaptive_buffer_size(500 * 1024), ADAPTIVE_BUFFER_SMALL);
    }

    #[test]
    fn adaptive_size_just_below_small_threshold() {
        // 1 MB - 1 byte: still in the small range.
        assert_eq!(adaptive_buffer_size(1024 * 1024 - 1), ADAPTIVE_BUFFER_SMALL);
    }

    #[test]
    fn adaptive_size_at_small_threshold() {
        // Exactly 1 MB: enters the medium range.
        assert_eq!(adaptive_buffer_size(1024 * 1024), ADAPTIVE_BUFFER_MEDIUM);
    }

    #[test]
    fn adaptive_size_medium_file() {
        // 10 MB file should get a 128 KB buffer.
        assert_eq!(
            adaptive_buffer_size(10 * 1024 * 1024),
            ADAPTIVE_BUFFER_MEDIUM
        );
    }

    #[test]
    fn adaptive_size_just_below_medium_threshold() {
        // 64 MB - 1 byte: still in the medium range.
        assert_eq!(
            adaptive_buffer_size(64 * 1024 * 1024 - 1),
            ADAPTIVE_BUFFER_MEDIUM
        );
    }

    #[test]
    fn adaptive_size_at_medium_threshold() {
        // Exactly 64 MB: enters the large range.
        assert_eq!(
            adaptive_buffer_size(64 * 1024 * 1024),
            ADAPTIVE_BUFFER_LARGE
        );
    }

    #[test]
    fn adaptive_size_large_file() {
        // 100 MB file should get a 512 KB buffer.
        assert_eq!(
            adaptive_buffer_size(100 * 1024 * 1024),
            ADAPTIVE_BUFFER_LARGE
        );
    }

    #[test]
    fn adaptive_size_very_large_file() {
        // 1 GB file should get a 512 KB buffer.
        assert_eq!(
            adaptive_buffer_size(1024 * 1024 * 1024),
            ADAPTIVE_BUFFER_LARGE
        );
    }

    #[test]
    fn adaptive_size_huge_file() {
        // 100 GB file should get a 512 KB buffer.
        assert_eq!(
            adaptive_buffer_size(100 * 1024 * 1024 * 1024),
            ADAPTIVE_BUFFER_LARGE
        );
    }

    #[test]
    fn adaptive_size_max_u64() {
        // Maximum possible file size should still return the large buffer.
        assert_eq!(adaptive_buffer_size(u64::MAX), ADAPTIVE_BUFFER_LARGE);
    }

    #[test]
    fn adaptive_size_monotonically_non_decreasing() {
        // Buffer sizes should never decrease as file size increases.
        let file_sizes: Vec<u64> = vec![
            0,
            1,
            1024,
            64 * 1024 - 1,
            64 * 1024,
            512 * 1024,
            1024 * 1024 - 1,
            1024 * 1024,
            32 * 1024 * 1024,
            64 * 1024 * 1024 - 1,
            64 * 1024 * 1024,
            1024 * 1024 * 1024,
        ];
        let mut prev_size = 0;
        for &file_size in &file_sizes {
            let buf_size = adaptive_buffer_size(file_size);
            assert!(
                buf_size >= prev_size,
                "buffer size decreased from {prev_size} to {buf_size} at file size {file_size}"
            );
            prev_size = buf_size;
        }
    }

    #[test]
    fn adaptive_size_constants_are_powers_of_two() {
        // I/O buffers should be powers of two for optimal alignment.
        assert!(ADAPTIVE_BUFFER_TINY.is_power_of_two());
        assert!(ADAPTIVE_BUFFER_SMALL.is_power_of_two());
        assert!(ADAPTIVE_BUFFER_MEDIUM.is_power_of_two());
        assert!(ADAPTIVE_BUFFER_LARGE.is_power_of_two());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn adaptive_size_constants_ordered() {
        assert!(ADAPTIVE_BUFFER_TINY < ADAPTIVE_BUFFER_SMALL);
        assert!(ADAPTIVE_BUFFER_SMALL < ADAPTIVE_BUFFER_MEDIUM);
        assert!(ADAPTIVE_BUFFER_MEDIUM < ADAPTIVE_BUFFER_LARGE);
    }

    #[test]
    fn adaptive_size_medium_equals_default_buffer() {
        // The medium adaptive size should match the default COPY_BUFFER_SIZE
        // so the pool can reuse buffers for medium-sized files.
        assert_eq!(ADAPTIVE_BUFFER_MEDIUM, COPY_BUFFER_SIZE);
    }

    #[test]
    fn acquire_adaptive_from_uses_pool_for_medium_files() {
        // For files in the medium range, the adaptive size matches the pool's
        // default buffer size, so the buffer should come from the pool.
        let pool = Arc::new(BufferPool::new(4));

        // Pre-populate the pool with a buffer
        {
            let _buffer = BufferPool::acquire_from(Arc::clone(&pool));
            // buffer is returned on drop
        }
        assert_eq!(pool.available(), 1);

        // Acquire adaptively for a medium file -- should reuse the pooled buffer
        let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 10 * 1024 * 1024);
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
        assert_eq!(pool.available(), 0); // was taken from pool
    }

    #[test]
    fn acquire_adaptive_from_allocates_small_buffer_for_tiny_file() {
        let pool = Arc::new(BufferPool::new(4));
        let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
        assert_eq!(buffer.len(), ADAPTIVE_BUFFER_TINY);
    }

    #[test]
    fn acquire_adaptive_from_allocates_small_buffer_for_small_file() {
        let pool = Arc::new(BufferPool::new(4));
        let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 500 * 1024);
        assert_eq!(buffer.len(), ADAPTIVE_BUFFER_SMALL);
    }

    #[test]
    fn acquire_adaptive_from_allocates_large_buffer_for_large_file() {
        let pool = Arc::new(BufferPool::new(4));
        let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 100 * 1024 * 1024);
        assert_eq!(buffer.len(), ADAPTIVE_BUFFER_LARGE);
    }

    #[test]
    fn acquire_adaptive_from_returns_buffer_to_pool() {
        // Verify that adaptively-sized buffers are still returned to the pool
        // (resized to pool default) when dropped.
        let pool = Arc::new(BufferPool::new(4));
        assert_eq!(pool.available(), 0);

        {
            let _buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
            // tiny buffer is active
        }
        // After drop, buffer should be returned and resized to pool default
        assert_eq!(pool.available(), 1);

        // Next acquire from pool should get a buffer of the default size
        let buffer = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    }

    #[test]
    fn acquire_adaptive_from_large_buffer_returns_resized() {
        // Even a 512 KB buffer gets resized to the default on return.
        let pool = Arc::new(BufferPool::new(4));
        {
            let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 100 * 1024 * 1024);
            assert_eq!(buffer.len(), ADAPTIVE_BUFFER_LARGE);
        }
        assert_eq!(pool.available(), 1);

        // The returned buffer should be at the pool's default size
        let buffer = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    }
}
