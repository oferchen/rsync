//! Thread-safe, lock-free buffer pool for reusing I/O buffers.
//!
//! This module provides a [`BufferPool`] that reduces allocation overhead during
//! file copy operations by reusing fixed-size buffers. Buffers are automatically
//! returned to the pool when the RAII guard ([`BufferGuard`] or
//! [`BorrowedBufferGuard`]) is dropped.
//!
//! # Adaptive Buffer Sizing
//!
//! The [`adaptive_buffer_size`] function selects an appropriate I/O buffer size
//! based on the file being transferred. Small files use smaller buffers to reduce
//! memory overhead, while large files use larger buffers for better throughput.
//! Use [`BufferPool::acquire_adaptive_from`] to acquire a buffer sized to match
//! the file being transferred.
//!
//! # Design
//!
//! The pool uses [`crossbeam_queue::ArrayQueue`], a bounded, lock-free MPMC
//! queue backed by a contiguous array. Buffers are pushed when released and
//! popped when acquired. Because `ArrayQueue` is array-backed, recently returned
//! buffers tend to be reused first, providing reasonable cache locality.
//!
//! # Contention Characteristics
//!
//! All pool operations (acquire and return) use lock-free compare-and-swap (CAS)
//! rather than a mutex. Under typical rsync workloads - where rayon worker
//! threads each process one file at a time - contention is negligible because
//! acquire and return calls are separated by the full duration of a file copy.
//!
//! Under extreme contention (many threads acquiring and returning buffers in
//! tight loops), CAS retries may increase slightly, but the operations remain
//! wait-free in practice. The pool never blocks: when empty, it allocates a
//! fresh buffer; when full, returned buffers are simply dropped. This means
//! contention affects allocation frequency rather than latency.
//!
//! If per-thread buffer pools become necessary for very high thread counts,
//! the `ArrayQueue` can be replaced with thread-local storage without changing
//! the guard API.
//!
//! # RAII Guard Pattern
//!
//! Buffers are never handed out directly. Instead, callers receive an RAII guard
//! that derefs to `[u8]` and automatically returns the buffer to the pool on
//! drop. Two guard variants are provided:
//!
//! - [`BufferGuard`] - holds an `Arc<BufferPool>`, decoupling the buffer
//!   lifetime from the pool borrow. Use this when the pool is part of a larger
//!   struct that needs to be mutably borrowed while a buffer is checked out.
//! - [`BorrowedBufferGuard`] - borrows the pool by reference, tying the guard
//!   lifetime to the pool. Lighter weight when the borrow checker allows it.
//!
//! Both guards use an internal `Option<Vec<u8>>` with take-on-drop semantics:
//! the `Drop` impl calls `Option::take` to move the buffer out, then passes it
//! to `BufferPool::return_buffer`. This pattern ensures the buffer is returned
//! exactly once, even if the guard is dropped during a panic unwind.
//!
//! # Ownership Model
//!
//! The pool is typically wrapped in [`Arc`](std::sync::Arc) so that
//! [`BufferGuard`] instances can hold an owned reference, avoiding borrow
//! checker issues when the pool is part of a larger context struct.
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

use std::sync::Arc;

use crossbeam_queue::ArrayQueue;

use super::COPY_BUFFER_SIZE;

mod allocator;
mod global;
mod guard;
pub use allocator::{BufferAllocator, DefaultAllocator};
pub use global::{global_buffer_pool, init_global_buffer_pool, GlobalBufferPoolConfig};
pub use guard::{BorrowedBufferGuard, BufferGuard};

/// Buffer size for files smaller than 64 KB (8 KB).
pub const ADAPTIVE_BUFFER_TINY: usize = super::ADAPTIVE_BUFFER_TINY;
/// Buffer size for files in the 64 KB .. 1 MB range (32 KB).
pub const ADAPTIVE_BUFFER_SMALL: usize = super::ADAPTIVE_BUFFER_SMALL;
/// Buffer size for files in the 1 MB .. 64 MB range (128 KB).
pub const ADAPTIVE_BUFFER_MEDIUM: usize = super::ADAPTIVE_BUFFER_MEDIUM;
/// Buffer size for files in the 64 MB .. 256 MB range (512 KB).
pub const ADAPTIVE_BUFFER_LARGE: usize = super::ADAPTIVE_BUFFER_LARGE;
/// Buffer size for files 256 MB and larger (1 MB).
pub const ADAPTIVE_BUFFER_HUGE: usize = super::ADAPTIVE_BUFFER_HUGE;

/// Selects an I/O buffer size appropriate for the given file size.
///
/// The returned size balances memory consumption against throughput:
///
/// | File size          | Buffer size |
/// |--------------------|-------------|
/// | < 64 KB            | 8 KB        |
/// | 64 KB .. < 1 MB    | 32 KB       |
/// | 1 MB .. < 64 MB    | 128 KB      |
/// | 64 MB .. < 256 MB  | 512 KB      |
/// | >= 256 MB          | 1 MB        |
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
/// assert_eq!(adaptive_buffer_size(1_000_000_000), 1024 * 1024);
/// ```
#[must_use]
pub const fn adaptive_buffer_size(file_size: u64) -> usize {
    super::adaptive_buffer_size(file_size)
}

/// A thread-safe, lock-free pool of reusable I/O buffers.
///
/// Reduces allocation overhead during file copy operations by maintaining
/// a bounded set of buffers that can be reused across operations. Internally
/// backed by [`crossbeam_queue::ArrayQueue`], all acquire and return
/// operations are lock-free CAS loops with no mutex overhead.
///
/// # Capacity
///
/// The pool has a maximum capacity to prevent unbounded memory growth.
/// When the pool is full, returning a buffer simply drops it.
/// When the pool is empty, acquiring allocates a fresh buffer rather than
/// blocking. This non-blocking guarantee means contention only affects
/// allocation frequency, never caller latency.
///
/// # Buffer Lifecycle
///
/// 1. **Acquire** - pop a buffer from the queue, or allocate if empty.
/// 2. **Use** - caller reads/writes through the RAII guard's `Deref`/`DerefMut`.
/// 3. **Return** - guard's `Drop` impl passes the buffer back via
///    [`return_buffer`](Self::return_buffer), which restores its length to
///    the pool default (without zeroing) and pushes it onto the queue.
#[derive(Debug)]
pub struct BufferPool<A: BufferAllocator = DefaultAllocator> {
    /// Lock-free bounded queue of available buffers.
    buffers: ArrayQueue<Vec<u8>>,
    /// Size of each buffer in bytes.
    buffer_size: usize,
    /// Allocation strategy for creating and disposing of buffers.
    allocator: A,
}

impl BufferPool {
    /// Creates a new buffer pool with the specified maximum capacity.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation. To supply a custom
    /// allocator, use [`with_allocator`](Self::with_allocator).
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
            buffers: ArrayQueue::new(max_buffers),
            buffer_size: COPY_BUFFER_SIZE,
            allocator: DefaultAllocator,
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: ArrayQueue::new(max_buffers),
            buffer_size,
            allocator: DefaultAllocator,
        }
    }
}

impl<A: BufferAllocator> BufferPool<A> {
    /// Creates a new buffer pool with a custom allocator.
    ///
    /// This is the fully general constructor. The allocator controls how
    /// buffers are created ([`BufferAllocator::allocate`]) and how excess
    /// buffers are disposed ([`BufferAllocator::deallocate`]).
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    /// * `allocator`   - The allocation strategy to use.
    #[must_use]
    pub fn with_allocator(max_buffers: usize, buffer_size: usize, allocator: A) -> Self {
        Self {
            buffers: ArrayQueue::new(max_buffers),
            buffer_size,
            allocator,
        }
    }

    /// Acquires a buffer from the pool using an Arc reference.
    ///
    /// This is the preferred method when the pool is part of a larger struct
    /// that needs to be mutably borrowed while the buffer is in use.
    ///
    /// Returns a pooled buffer if available, otherwise allocates a new one
    /// via the pool's [`BufferAllocator`]. The returned [`BufferGuard`]
    /// automatically returns the buffer to the pool when dropped.
    #[must_use]
    pub fn acquire_from(pool: Arc<Self>) -> BufferGuard<A> {
        let buffer = pool
            .buffers
            .pop()
            .unwrap_or_else(|| pool.allocator.allocate(pool.buffer_size));

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
    #[must_use]
    pub fn acquire_adaptive_from(pool: Arc<Self>, file_size: u64) -> BufferGuard<A> {
        let desired = adaptive_buffer_size(file_size);

        if desired == pool.buffer_size {
            // Fast path: adaptive size matches pool default - reuse pooled buffers.
            return Self::acquire_from(pool);
        }

        // Slow path: non-standard size - allocate a fresh buffer.
        // On drop the guard will pass it through `return_buffer` which
        // resizes it to the pool default before returning it.
        BufferGuard {
            buffer: Some(pool.allocator.allocate(desired)),
            pool,
        }
    }

    /// Acquires a buffer from the pool (borrows self).
    ///
    /// **Note:** This method returns a guard with a lifetime tied to `self`.
    /// Use [`acquire_from`](Self::acquire_from) when the pool is part of a
    /// larger context that needs to be mutably borrowed.
    #[must_use]
    pub fn acquire(&self) -> BorrowedBufferGuard<'_, A> {
        let buffer = self
            .buffers
            .pop()
            .unwrap_or_else(|| self.allocator.allocate(self.buffer_size));

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
    /// If the pool is at capacity, the buffer is disposed via
    /// [`BufferAllocator::deallocate`].
    #[allow(unsafe_code)]
    fn return_buffer(&self, mut buffer: Vec<u8>) {
        if buffer.capacity() < self.buffer_size {
            // Small adaptive buffer - replace with fresh allocation at pool size.
            buffer = Vec::with_capacity(self.buffer_size);
        }
        // SAFETY: capacity >= self.buffer_size is guaranteed by the branch
        // above (fresh allocation) or by the original allocation (same-size
        // or larger adaptive buffer). The stale contents will be fully
        // overwritten by the next Read::read() before being consumed.
        // This avoids the expensive `resize(size, 0)` memset that was the
        // #1 CPU hotspot (26% of runtime per flamegraph profiling).
        unsafe { buffer.set_len(self.buffer_size) };

        // ArrayQueue::push returns Err when full - dispose via the allocator.
        if let Err(buffer) = self.buffers.push(buffer) {
            self.allocator.deallocate(buffer);
        }
    }

    /// Returns the number of buffers currently in the pool.
    ///
    /// This is primarily useful for testing and monitoring.
    #[must_use]
    pub fn available(&self) -> usize {
        self.buffers.len()
    }

    /// Returns the maximum number of buffers the pool will retain.
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.buffers.capacity()
    }

    /// Returns the size of each buffer in bytes.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// Returns a reference to the pool's allocator.
    #[must_use]
    pub fn allocator(&self) -> &A {
        &self.allocator
    }
}

impl Default for BufferPool<DefaultAllocator> {
    /// Creates a buffer pool sized for the host's available parallelism.
    ///
    /// Capacity is set to the number of hardware threads
    /// ([`std::thread::available_parallelism`]), falling back to 4 if
    /// detection fails. This matches the typical rayon thread pool size,
    /// ensuring one pooled buffer per worker thread.
    fn default() -> Self {
        let max_buffers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        Self::new(max_buffers)
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
    fn adaptive_size_at_large_threshold() {
        // Exactly 256 MB: enters the huge range.
        assert_eq!(
            adaptive_buffer_size(256 * 1024 * 1024),
            ADAPTIVE_BUFFER_HUGE
        );
    }

    #[test]
    fn adaptive_size_very_large_file() {
        // 1 GB file should get a 1 MB buffer.
        assert_eq!(
            adaptive_buffer_size(1024 * 1024 * 1024),
            ADAPTIVE_BUFFER_HUGE
        );
    }

    #[test]
    fn adaptive_size_huge_file() {
        // 100 GB file should get a 1 MB buffer.
        assert_eq!(
            adaptive_buffer_size(100 * 1024 * 1024 * 1024),
            ADAPTIVE_BUFFER_HUGE
        );
    }

    #[test]
    fn adaptive_size_max_u64() {
        // Maximum possible file size should still return the huge buffer.
        assert_eq!(adaptive_buffer_size(u64::MAX), ADAPTIVE_BUFFER_HUGE);
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
            256 * 1024 * 1024 - 1,
            256 * 1024 * 1024,
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
        assert!(ADAPTIVE_BUFFER_HUGE.is_power_of_two());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn adaptive_size_constants_ordered() {
        assert!(ADAPTIVE_BUFFER_TINY < ADAPTIVE_BUFFER_SMALL);
        assert!(ADAPTIVE_BUFFER_SMALL < ADAPTIVE_BUFFER_MEDIUM);
        assert!(ADAPTIVE_BUFFER_MEDIUM < ADAPTIVE_BUFFER_LARGE);
        assert!(ADAPTIVE_BUFFER_LARGE < ADAPTIVE_BUFFER_HUGE);
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

    // ---------------------------------------------------------------
    // Memory pressure and concurrency tests
    // ---------------------------------------------------------------

    #[test]
    fn pool_reuses_buffers_under_sequential_pressure() {
        // Allocate and return many buffers sequentially.
        // The pool should reuse buffers so that at most max_buffers
        // are retained, regardless of how many iterations run.
        let pool = Arc::new(BufferPool::new(4));

        for _ in 0..1_000 {
            let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
            buf[0] = 0xAB;
        }

        // After all guards are dropped the pool holds at most max_buffers.
        assert!(pool.available() <= 4);
        // At least one buffer was returned (proves reuse path was exercised).
        assert!(pool.available() >= 1);
    }

    #[test]
    fn pool_size_stays_bounded_under_burst_allocation() {
        // Acquire many buffers simultaneously (simulating a burst), then
        // release them all. The pool must not grow beyond max_buffers.
        let max = 4;
        let pool = Arc::new(BufferPool::new(max));

        let guards: Vec<_> = (0..64)
            .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
            .collect();

        // While all 64 buffers are checked out the pool is empty.
        assert_eq!(pool.available(), 0);

        // Drop all guards - only max_buffers should be retained.
        drop(guards);
        assert_eq!(pool.available(), max);
    }

    #[test]
    fn empty_pool_allocates_fresh_buffer() {
        // When no buffers are available the pool creates a new one
        // rather than blocking.
        let pool = Arc::new(BufferPool::new(2));
        assert_eq!(pool.available(), 0);

        let buf = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(buf.len(), COPY_BUFFER_SIZE);
        // Pool is still empty because the buffer is checked out.
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn drop_returns_buffer_to_pool() {
        // Explicitly verify the BufferGuard Drop impl returns the buffer.
        let pool = Arc::new(BufferPool::new(4));
        assert_eq!(pool.available(), 0);

        let guard = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);

        drop(guard);
        assert_eq!(pool.available(), 1);
    }

    #[test]
    fn borrowed_guard_drop_returns_buffer_to_pool() {
        // Same verification for BorrowedBufferGuard.
        let pool = BufferPool::new(4);
        assert_eq!(pool.available(), 0);

        let guard = pool.acquire();
        assert_eq!(pool.available(), 0);

        drop(guard);
        assert_eq!(pool.available(), 1);
    }

    #[test]
    fn concurrent_checkout_return_from_multiple_threads() {
        // Hammer the pool from many threads with rapid acquire/release
        // cycles. Validates absence of deadlocks, data races, and that
        // the pool invariant (available <= max_buffers) always holds.
        let pool = Arc::new(BufferPool::new(8));
        let iterations = 500;
        let thread_count = 16;

        let handles: Vec<_> = (0..thread_count)
            .map(|id| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for i in 0..iterations {
                        let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                        // Write a recognizable pattern to detect cross-thread corruption.
                        buf[0] = (id & 0xFF) as u8;
                        buf[1] = (i & 0xFF) as u8;
                        assert_eq!(buf[0], (id & 0xFF) as u8);
                        // Guard dropped here - buffer returns to pool.
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("worker thread panicked");
        }

        assert!(pool.available() <= 8);
    }

    #[test]
    fn concurrent_mixed_guard_types() {
        // Exercise both Arc-based and borrow-based guards from threads.
        // The borrowed guard can only be used within a single thread
        // (lifetime tied to pool), but we test that concurrent Arc-based
        // and sequential borrow-based access both work correctly.
        let pool = Arc::new(BufferPool::new(4));

        // Spawn threads using Arc-based acquire_from.
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for _ in 0..200 {
                        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        assert!(pool.available() <= 4);

        // Now use borrowed guard on the main thread.
        let available_before = pool.available();
        {
            let _buf = pool.acquire();
        }
        // Buffer was returned; available count should be at least what it was.
        assert!(pool.available() >= available_before);
    }

    #[test]
    fn concurrent_held_buffers_force_new_allocations() {
        // Hold some buffers while other threads acquire and release.
        // Verifies the pool allocates fresh buffers when empty and that
        // held guards do not interfere with new acquisitions.
        let pool = Arc::new(BufferPool::new(2));

        // Hold 2 buffers on the main thread, exhausting the pool.
        let _held1 = BufferPool::acquire_from(Arc::clone(&pool));
        let _held2 = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);

        // Spawn threads that acquire and release buffers - they all get
        // fresh allocations since the pool is empty and 2 buffers are held.
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                        buf[0] = 0xFF;
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        // Pool accepted returns up to max_buffers while threads ran.
        assert!(pool.available() <= 2);

        // Release held buffers. Pool is already at capacity so excess
        // buffers are dropped.
        drop(_held1);
        drop(_held2);
        assert!(pool.available() <= 2);
    }

    #[test]
    fn adaptive_buffers_returned_under_concurrent_pressure() {
        // Mix adaptive and default-sized buffer acquisitions concurrently.
        // All returned buffers should be resized to pool default.
        let pool = Arc::new(BufferPool::new(8));

        let file_sizes: Vec<u64> = vec![
            512,               // tiny
            100 * 1024,        // small
            10 * 1024 * 1024,  // medium (matches pool default)
            100 * 1024 * 1024, // large
            500 * 1024 * 1024, // huge
        ];

        let handles: Vec<_> = file_sizes
            .into_iter()
            .map(|size| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let buf = BufferPool::acquire_adaptive_from(Arc::clone(&pool), size);
                        assert!(!buf.is_empty());
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        assert!(pool.available() <= 8);

        // Every buffer in the pool should now be at the default size.
        for _ in 0..pool.available() {
            let buf = BufferPool::acquire_from(Arc::clone(&pool));
            assert_eq!(buf.len(), COPY_BUFFER_SIZE);
        }
    }

    #[test]
    fn repeated_acquire_release_cycle_reuses_same_buffers() {
        // Verify the pool actually recycles buffers by checking that
        // the pool count stabilizes rather than growing.
        let pool = Arc::new(BufferPool::new(2));

        // First cycle - buffers are freshly allocated.
        {
            let _a = BufferPool::acquire_from(Arc::clone(&pool));
            let _b = BufferPool::acquire_from(Arc::clone(&pool));
        }
        assert_eq!(pool.available(), 2);

        // Second cycle - buffers should come from the pool.
        {
            let _a = BufferPool::acquire_from(Arc::clone(&pool));
            assert_eq!(pool.available(), 1);
            let _b = BufferPool::acquire_from(Arc::clone(&pool));
            assert_eq!(pool.available(), 0);
        }
        // All returned.
        assert_eq!(pool.available(), 2);

        // After 100 more cycles the pool still holds exactly max_buffers.
        for _ in 0..100 {
            let _a = BufferPool::acquire_from(Arc::clone(&pool));
            let _b = BufferPool::acquire_from(Arc::clone(&pool));
        }
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn zero_capacity_pool_never_retains_buffers() {
        // Edge case: a pool with max_buffers=0 always allocates fresh
        // buffers and never retains returned ones.
        let pool = Arc::new(BufferPool::new(0));

        {
            let buf = BufferPool::acquire_from(Arc::clone(&pool));
            assert_eq!(buf.len(), COPY_BUFFER_SIZE);
        }
        assert_eq!(pool.available(), 0);

        // Even after many cycles, nothing is retained.
        for _ in 0..50 {
            let _buf = BufferPool::acquire_from(Arc::clone(&pool));
        }
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn single_capacity_pool_reuses_one_buffer() {
        // A pool with capacity 1 should cycle a single buffer.
        let pool = Arc::new(BufferPool::new(1));

        {
            let _buf = BufferPool::acquire_from(Arc::clone(&pool));
        }
        assert_eq!(pool.available(), 1);

        // Acquire two simultaneously - second must be a fresh allocation.
        let a = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);
        let b = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);

        drop(a);
        assert_eq!(pool.available(), 1);
        // Dropping b exceeds capacity - it should be discarded.
        drop(b);
        assert_eq!(pool.available(), 1);
    }

    // ---------------------------------------------------------------
    // Custom allocator tests
    // ---------------------------------------------------------------

    /// A test-only allocator that counts allocations and deallocations.
    #[derive(Debug)]
    struct TrackingAllocator {
        alloc_count: std::sync::atomic::AtomicUsize,
        dealloc_count: std::sync::atomic::AtomicUsize,
    }

    impl TrackingAllocator {
        fn new() -> Self {
            Self {
                alloc_count: std::sync::atomic::AtomicUsize::new(0),
                dealloc_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn alloc_count(&self) -> usize {
            self.alloc_count
                .load(std::sync::atomic::Ordering::Relaxed)
        }

        fn dealloc_count(&self) -> usize {
            self.dealloc_count
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl BufferAllocator for TrackingAllocator {
        fn allocate(&self, size: usize) -> Vec<u8> {
            self.alloc_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![0u8; size]
        }

        fn deallocate(&self, _buffer: Vec<u8>) {
            self.dealloc_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn with_allocator_uses_custom_allocator() {
        let pool = BufferPool::with_allocator(4, 1024, TrackingAllocator::new());
        assert_eq!(pool.buffer_size(), 1024);
        assert_eq!(pool.allocator().alloc_count(), 0);

        let buf = pool.acquire();
        assert_eq!(buf.len(), 1024);
        assert_eq!(pool.allocator().alloc_count(), 1);
    }

    #[test]
    fn custom_allocator_deallocate_called_on_overflow() {
        // Pool with capacity 1 - second returned buffer triggers deallocate.
        let pool = Arc::new(BufferPool::with_allocator(
            1,
            512,
            TrackingAllocator::new(),
        ));

        let a = BufferPool::acquire_from(Arc::clone(&pool));
        let b = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.allocator().alloc_count(), 2);

        drop(a); // returned to pool (pool was empty)
        assert_eq!(pool.available(), 1);
        assert_eq!(pool.allocator().dealloc_count(), 0);

        drop(b); // pool is full - deallocate is called
        assert_eq!(pool.available(), 1);
        assert_eq!(pool.allocator().dealloc_count(), 1);
    }

    #[test]
    fn custom_allocator_with_arc_guards() {
        let pool = Arc::new(BufferPool::with_allocator(
            4,
            2048,
            TrackingAllocator::new(),
        ));

        {
            let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
            buf[0] = 0xAB;
            assert_eq!(buf[0], 0xAB);
        }

        assert_eq!(pool.available(), 1);
        assert_eq!(pool.allocator().alloc_count(), 1);
    }

    #[test]
    fn custom_allocator_adaptive_acquire() {
        let pool = Arc::new(BufferPool::with_allocator(
            4,
            COPY_BUFFER_SIZE,
            TrackingAllocator::new(),
        ));

        // Tiny file - non-standard size, allocator should be used
        let buf = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
        assert_eq!(buf.len(), ADAPTIVE_BUFFER_TINY);
        assert_eq!(pool.allocator().alloc_count(), 1);
    }

    #[test]
    fn allocator_accessor_returns_reference() {
        let pool = BufferPool::with_allocator(2, 256, TrackingAllocator::new());
        let _alloc: &TrackingAllocator = pool.allocator();
        assert_eq!(_alloc.alloc_count(), 0);
    }
}
