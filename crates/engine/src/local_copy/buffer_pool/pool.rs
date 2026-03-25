//! Core [`BufferPool`] implementation.
//!
//! Provides the thread-safe, lock-free buffer pool backed by
//! [`crossbeam_queue::ArrayQueue`]. See the [module-level documentation](super)
//! for design rationale and usage patterns.

use std::sync::Arc;

use crossbeam_queue::ArrayQueue;

use super::allocator::{BufferAllocator, DefaultAllocator};
use super::guard::{BorrowedBufferGuard, BufferGuard};
use super::memory_cap::MemoryCap;
use super::{COPY_BUFFER_SIZE, adaptive_buffer_size};

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
/// # Memory Cap
///
/// An optional hard memory cap can be set via [`with_memory_cap`](Self::with_memory_cap).
/// When configured, the pool tracks outstanding (checked-out) memory and
/// blocks `acquire` calls that would exceed the cap until a buffer is
/// returned (backpressure). Use `try_acquire` / `try_acquire_from` for
/// non-blocking semantics that return `None` at the cap.
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
    ///
    /// `ArrayQueue` requires non-zero capacity, so the actual queue is
    /// created with `max(1, requested_capacity)`. The `requested_capacity`
    /// field tracks the caller's intent - when zero, `return_buffer`
    /// deallocates immediately instead of pushing onto the queue.
    buffers: ArrayQueue<Vec<u8>>,
    /// Caller-requested maximum number of buffers to retain.
    ///
    /// May be zero, meaning the pool never retains returned buffers
    /// (every allocation is fresh). Distinct from `buffers.capacity()`
    /// which is always >= 1 due to `ArrayQueue`'s invariant.
    requested_capacity: usize,
    /// Size of each buffer in bytes.
    buffer_size: usize,
    /// Allocation strategy for creating and disposing of buffers.
    allocator: A,
    /// Optional hard memory cap with backpressure.
    memory_cap: Option<MemoryCap>,
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
            // ArrayQueue requires capacity >= 1; clamp here, enforce
            // zero-capacity semantics via `requested_capacity` in return_buffer.
            buffers: ArrayQueue::new(max_buffers.max(1)),
            requested_capacity: max_buffers,
            buffer_size: COPY_BUFFER_SIZE,
            allocator: DefaultAllocator,
            memory_cap: None,
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain. Pass `0` for a
    ///   pool that never retains buffers (every acquire allocates fresh).
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: ArrayQueue::new(max_buffers.max(1)),
            requested_capacity: max_buffers,
            buffer_size,
            allocator: DefaultAllocator,
            memory_cap: None,
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
            buffers: ArrayQueue::new(max_buffers.max(1)),
            requested_capacity: max_buffers,
            buffer_size,
            allocator,
            memory_cap: None,
        }
    }

    /// Sets a hard memory cap on the pool.
    ///
    /// When the total memory outstanding (buffers checked out by callers)
    /// reaches `max_bytes`, subsequent `acquire` and `acquire_from` calls
    /// block until a buffer is returned (backpressure). Use `try_acquire`
    /// or `try_acquire_from` for non-blocking semantics that return `None`
    /// when the cap is reached.
    ///
    /// The cap applies to outstanding (checked-out) buffers only. Buffers
    /// sitting idle in the pool do not count against the cap because they
    /// are available for immediate reuse without new allocation.
    ///
    /// # Panics
    ///
    /// Panics if `max_bytes` is zero.
    #[must_use]
    pub fn with_memory_cap(mut self, max_bytes: usize) -> Self {
        self.memory_cap = Some(MemoryCap::new(max_bytes));
        self
    }

    /// Acquires a buffer from the pool using an Arc reference.
    ///
    /// This is the preferred method when the pool is part of a larger struct
    /// that needs to be mutably borrowed while the buffer is in use.
    ///
    /// Returns a pooled buffer if available, otherwise allocates a new one
    /// via the pool's [`BufferAllocator`]. The returned [`BufferGuard`]
    /// automatically returns the buffer to the pool when dropped.
    ///
    /// If a memory cap is configured and the outstanding memory equals or
    /// exceeds the cap, this method blocks until a buffer is returned by
    /// another thread (backpressure). Use [`try_acquire_from`](Self::try_acquire_from)
    /// for a non-blocking alternative.
    #[must_use]
    pub fn acquire_from(pool: Arc<Self>) -> BufferGuard<A> {
        pool.wait_for_memory_capacity(pool.buffer_size);
        let buffer = pool
            .buffers
            .pop()
            .unwrap_or_else(|| pool.allocator.allocate(pool.buffer_size));

        pool.track_checkout(buffer.len());
        BufferGuard {
            buffer: Some(buffer),
            pool,
        }
    }

    /// Tries to acquire a buffer without blocking.
    ///
    /// Returns `None` if a memory cap is configured and outstanding memory
    /// is at or above the cap. Otherwise behaves identically to
    /// [`acquire_from`](Self::acquire_from).
    #[must_use]
    pub fn try_acquire_from(pool: Arc<Self>) -> Option<BufferGuard<A>> {
        if !pool.try_reserve_memory(pool.buffer_size) {
            return None;
        }
        let buffer = pool
            .buffers
            .pop()
            .unwrap_or_else(|| pool.allocator.allocate(pool.buffer_size));

        pool.track_checkout(buffer.len());
        Some(BufferGuard {
            buffer: Some(buffer),
            pool,
        })
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
        pool.wait_for_memory_capacity(desired);
        let buffer = pool.allocator.allocate(desired);
        pool.track_checkout(buffer.len());
        BufferGuard {
            buffer: Some(buffer),
            pool,
        }
    }

    /// Acquires a buffer from the pool (borrows self).
    ///
    /// **Note:** This method returns a guard with a lifetime tied to `self`.
    /// Use [`acquire_from`](Self::acquire_from) when the pool is part of a
    /// larger context that needs to be mutably borrowed.
    ///
    /// Blocks if a memory cap is configured and the cap is reached.
    #[must_use]
    pub fn acquire(&self) -> BorrowedBufferGuard<'_, A> {
        self.wait_for_memory_capacity(self.buffer_size);
        let buffer = self
            .buffers
            .pop()
            .unwrap_or_else(|| self.allocator.allocate(self.buffer_size));

        self.track_checkout(buffer.len());
        BorrowedBufferGuard {
            buffer: Some(buffer),
            pool: self,
        }
    }

    /// Tries to acquire a buffer without blocking (borrows self).
    ///
    /// Returns `None` if a memory cap is configured and outstanding memory
    /// is at or above the cap.
    #[must_use]
    pub fn try_acquire(&self) -> Option<BorrowedBufferGuard<'_, A>> {
        if !self.try_reserve_memory(self.buffer_size) {
            return None;
        }
        let buffer = self
            .buffers
            .pop()
            .unwrap_or_else(|| self.allocator.allocate(self.buffer_size));

        self.track_checkout(buffer.len());
        Some(BorrowedBufferGuard {
            buffer: Some(buffer),
            pool: self,
        })
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
    ///
    /// When a memory cap is configured, outstanding bytes are decremented
    /// and any threads blocked in `acquire` are notified.
    #[allow(unsafe_code)]
    pub(super) fn return_buffer(&self, mut buffer: Vec<u8>) {
        let returned_len = buffer.len();

        // Zero-capacity pool: never retain buffers - deallocate immediately.
        if self.requested_capacity == 0 {
            self.allocator.deallocate(buffer);
            self.track_return(returned_len);
            return;
        }

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

        // Release outstanding memory and wake blocked acquirers.
        self.track_return(returned_len);
    }

    /// Returns the number of buffers currently in the pool.
    ///
    /// This is primarily useful for testing and monitoring.
    #[must_use]
    pub fn available(&self) -> usize {
        self.buffers.len()
    }

    /// Returns the maximum number of buffers the pool will retain.
    ///
    /// Returns `0` for a zero-capacity pool (never retains buffers).
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.requested_capacity
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

    /// Returns the number of bytes currently checked out (outstanding).
    ///
    /// Returns `0` if no memory cap is configured (no tracking overhead
    /// is incurred without a cap).
    #[must_use]
    pub fn memory_usage(&self) -> usize {
        self.memory_cap
            .as_ref()
            .map(|cap| cap.outstanding())
            .unwrap_or(0)
    }

    /// Returns the configured memory cap in bytes, or `None` if uncapped.
    #[must_use]
    pub fn memory_cap(&self) -> Option<usize> {
        self.memory_cap.as_ref().map(|cap| cap.limit())
    }

    /// Blocks until outstanding memory is below the cap.
    ///
    /// No-op when no memory cap is configured.
    fn wait_for_memory_capacity(&self, requested: usize) {
        if let Some(cap) = &self.memory_cap {
            cap.wait_for_capacity(requested);
        }
    }

    /// Returns `true` if the requested bytes can be allocated without
    /// exceeding the memory cap. Returns `true` unconditionally when no
    /// cap is configured.
    fn try_reserve_memory(&self, requested: usize) -> bool {
        match &self.memory_cap {
            Some(cap) => cap.try_reserve(requested),
            None => true,
        }
    }

    /// Records that `size` bytes have been checked out.
    fn track_checkout(&self, size: usize) {
        if let Some(cap) = &self.memory_cap {
            cap.track_checkout(size);
        }
    }

    /// Records that `size` bytes have been returned and wakes waiters.
    fn track_return(&self, size: usize) {
        if let Some(cap) = &self.memory_cap {
            cap.track_return(size);
        }
    }
}

impl Default for BufferPool<DefaultAllocator> {
    /// Creates a buffer pool sized for the host's available parallelism.
    ///
    /// Capacity is set to the number of hardware threads
    /// ([`std::thread::available_parallelism`]), falling back to 4 if
    /// detection fails. This matches the typical rayon thread pool size,
    /// ensuring one pooled buffer per worker thread. No memory cap is set.
    fn default() -> Self {
        let max_buffers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        Self::new(max_buffers)
    }
}
