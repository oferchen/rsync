//! Core [`BufferPool`] implementation.
//!
//! Provides the thread-safe, lock-free buffer pool backed by
//! [`crossbeam_queue::SegQueue`] with atomic length tracking. See the
//! [module-level documentation](super) for design rationale and usage patterns.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_queue::SegQueue;

use super::allocator::{BufferAllocator, DefaultAllocator};
use super::guard::{BorrowedBufferGuard, BufferGuard};
use super::memory_cap::MemoryCap;
use super::throughput::ThroughputTracker;
use super::{COPY_BUFFER_SIZE, adaptive_buffer_size};

/// A thread-safe, lock-free pool of reusable I/O buffers.
///
/// Reduces allocation overhead during file copy operations by maintaining
/// a set of buffers that can be reused across operations. Internally backed
/// by [`crossbeam_queue::SegQueue`] (an unbounded lock-free MPMC queue) with
/// an atomic length counter for soft capacity enforcement. All acquire and
/// return operations are lock-free CAS loops with no mutex overhead.
///
/// # Elastic Capacity
///
/// The pool has a *soft* maximum capacity (`max_buffers`). Under normal
/// operation, the pool retains up to `max_buffers` buffers. Under burst
/// conditions - when many threads return buffers simultaneously - the pool
/// absorbs the excess rather than deallocating, avoiding the pathological
/// case where buffers are freed and immediately reallocated. The pool
/// gradually drains back to `max_buffers` as excess buffers are acquired
/// without being replaced.
///
/// This elastic design eliminates the fixed-capacity `ArrayQueue` problem
/// where sizing the queue too small for the actual parallelism level causes
/// buffers to be deallocated on every return, defeating the pooling purpose.
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
    /// Lock-free unbounded queue of available buffers.
    ///
    /// Uses `SegQueue` instead of `ArrayQueue` to absorb burst returns
    /// without dropping buffers. The `pool_len` atomic tracks the current
    /// queue length for soft capacity enforcement.
    buffers: SegQueue<Vec<u8>>,
    /// Approximate number of buffers currently in the queue.
    ///
    /// Incremented on push, decremented on pop. Used for soft capacity
    /// enforcement in `return_buffer` - when `pool_len >= soft_capacity`,
    /// returned buffers are deallocated instead of queued.
    ///
    /// This count may briefly drift from the true queue length under
    /// concurrent access (a push increments before the buffer is visible
    /// to poppers, or vice versa), but the drift is bounded and harmless:
    /// the worst case is one extra allocation or one extra retained buffer.
    pool_len: AtomicUsize,
    /// Soft maximum number of buffers to retain.
    ///
    /// May be zero, meaning the pool never retains returned buffers
    /// (every allocation is fresh). Under burst conditions, the actual
    /// queue length may temporarily exceed this value - the pool drains
    /// naturally as buffers are acquired without replacement.
    soft_capacity: usize,
    /// Size of each buffer in bytes.
    buffer_size: usize,
    /// Allocation strategy for creating and disposing of buffers.
    allocator: A,
    /// Optional hard memory cap with backpressure.
    memory_cap: Option<MemoryCap>,
    /// Optional throughput tracker for dynamic buffer sizing.
    ///
    /// When present, the pool tracks transfer throughput via EMA and uses
    /// it to recommend buffer sizes via [`recommended_buffer_size`](Self::recommended_buffer_size).
    /// The tracker is only allocated when explicitly enabled via
    /// [`with_throughput_tracking`](Self::with_throughput_tracking).
    throughput: Option<ThroughputTracker>,
}

impl BufferPool {
    /// Creates a new buffer pool with the specified soft capacity.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation. To supply a custom
    /// allocator, use [`with_allocator`](Self::with_allocator).
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Soft maximum number of buffers to retain. Under
    ///   normal operation the pool holds at most this many buffers; under
    ///   burst conditions the pool may temporarily hold more.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pool = BufferPool::new(num_cpus::get());
    /// ```
    #[must_use]
    pub fn new(max_buffers: usize) -> Self {
        Self {
            buffers: SegQueue::new(),
            pool_len: AtomicUsize::new(0),
            soft_capacity: max_buffers,
            buffer_size: COPY_BUFFER_SIZE,
            allocator: DefaultAllocator,
            memory_cap: None,
            throughput: None,
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Soft maximum number of buffers to retain. Pass `0`
    ///   for a pool that never retains buffers (every acquire allocates fresh).
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: SegQueue::new(),
            pool_len: AtomicUsize::new(0),
            soft_capacity: max_buffers,
            buffer_size,
            allocator: DefaultAllocator,
            memory_cap: None,
            throughput: None,
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
            buffers: SegQueue::new(),
            pool_len: AtomicUsize::new(0),
            soft_capacity: max_buffers,
            buffer_size,
            allocator,
            memory_cap: None,
            throughput: None,
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

    /// Enables throughput tracking with the default EMA smoothing factor.
    ///
    /// When enabled, the pool maintains an EMA-based throughput estimate
    /// that can be queried via [`recommended_buffer_size`](Self::recommended_buffer_size).
    /// Callers record transfer samples via [`record_transfer`](Self::record_transfer)
    /// and the pool recommends a buffer size that targets ~10 ms of data
    /// per buffer, clamped between 4 KiB and 256 KiB (or memory_cap / 4).
    ///
    /// Throughput tracking is zero-cost when not enabled - no atomic
    /// operations or memory overhead are incurred.
    #[must_use]
    pub fn with_throughput_tracking(mut self) -> Self {
        self.throughput = Some(ThroughputTracker::new());
        self
    }

    /// Enables throughput tracking with a custom EMA smoothing factor.
    ///
    /// See [`with_throughput_tracking`](Self::with_throughput_tracking) for details.
    ///
    /// # Panics
    ///
    /// Panics if `alpha` is not in `(0.0, 1.0]`.
    #[must_use]
    pub fn with_throughput_tracking_alpha(mut self, alpha: f64) -> Self {
        self.throughput = Some(ThroughputTracker::with_alpha(alpha));
        self
    }

    /// Records a transfer sample for throughput tracking.
    ///
    /// No-op if throughput tracking is not enabled. This method is safe
    /// to call from any thread.
    pub fn record_transfer(&self, bytes: usize, duration: std::time::Duration) {
        if let Some(tracker) = &self.throughput {
            tracker.record_transfer(bytes, duration);
        }
    }

    /// Returns a recommended buffer size based on observed throughput.
    ///
    /// When throughput tracking is enabled, uses the EMA estimate to compute
    /// a buffer size targeting ~10 ms of data. The result is clamped between
    /// 4 KiB and the lesser of 256 KiB or `memory_cap / 4`.
    ///
    /// When tracking is disabled, returns the pool's configured `buffer_size`.
    #[must_use]
    pub fn recommended_buffer_size(&self) -> usize {
        match &self.throughput {
            Some(tracker) => {
                let max = self
                    .memory_cap
                    .as_ref()
                    .map(|cap| cap.limit() / 4)
                    .unwrap_or(super::throughput::MAX_BUFFER_SIZE);
                tracker.recommended_buffer_size(max)
            }
            None => self.buffer_size,
        }
    }

    /// Returns a reference to the throughput tracker, if enabled.
    #[must_use]
    pub fn throughput_tracker(&self) -> Option<&ThroughputTracker> {
        self.throughput.as_ref()
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
        pool.wait_and_reserve_memory(pool.buffer_size);
        let buffer = pool.pop_buffer();

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
        let buffer = pool.pop_buffer();

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
        pool.wait_and_reserve_memory(desired);
        let buffer = pool.allocator.allocate(desired);
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
        self.wait_and_reserve_memory(self.buffer_size);
        let buffer = self.pop_buffer();

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
        let buffer = self.pop_buffer();

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
    /// If the pool is at or above its soft capacity, the buffer is disposed
    /// via [`BufferAllocator::deallocate`]. Under burst conditions (many
    /// threads returning simultaneously), the pool may temporarily exceed
    /// the soft capacity because `pool_len` is checked non-atomically with
    /// the push - this is intentional and harmless.
    ///
    /// When a memory cap is configured, outstanding bytes are decremented
    /// and any threads blocked in `acquire` are notified.
    #[allow(unsafe_code)]
    pub(super) fn return_buffer(&self, mut buffer: Vec<u8>) {
        let returned_len = buffer.len();

        // Zero-capacity pool: never retain buffers - deallocate immediately.
        if self.soft_capacity == 0 {
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

        // Soft capacity check: allow the queue to absorb burst returns
        // even above soft_capacity. The Relaxed load is intentional - a
        // brief overcount is harmless and avoids CAS overhead on the hot
        // return path.
        let current = self.pool_len.load(Ordering::Relaxed);
        if current >= self.soft_capacity {
            self.allocator.deallocate(buffer);
        } else {
            self.buffers.push(buffer);
            self.pool_len.fetch_add(1, Ordering::Release);
        }

        // Release outstanding memory and wake blocked acquirers.
        self.track_return(returned_len);
    }

    /// Pops a buffer from the queue, or allocates a new one if empty.
    fn pop_buffer(&self) -> Vec<u8> {
        match self.buffers.pop() {
            Some(buffer) => {
                self.pool_len.fetch_sub(1, Ordering::Acquire);
                buffer
            }
            None => self.allocator.allocate(self.buffer_size),
        }
    }

    /// Returns the number of buffers currently in the pool.
    ///
    /// This is primarily useful for testing and monitoring. The value is
    /// approximate under concurrent access.
    #[must_use]
    pub fn available(&self) -> usize {
        self.pool_len.load(Ordering::Relaxed)
    }

    /// Returns the soft maximum number of buffers the pool will retain.
    ///
    /// Under burst conditions the pool may temporarily hold more buffers;
    /// it drains back to this level as buffers are acquired. Returns `0`
    /// for a zero-capacity pool (never retains buffers).
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.soft_capacity
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

    /// Atomically waits for and reserves `requested` bytes of capacity.
    ///
    /// When a memory cap is configured, blocks until outstanding memory
    /// plus `requested` is within the cap, then atomically increments
    /// outstanding. No-op when no cap is configured.
    fn wait_and_reserve_memory(&self, requested: usize) {
        if let Some(cap) = &self.memory_cap {
            cap.wait_and_reserve(requested);
        }
    }

    /// Tries to atomically reserve `requested` bytes without blocking.
    ///
    /// Returns `true` unconditionally when no cap is configured.
    fn try_reserve_memory(&self, requested: usize) -> bool {
        match &self.memory_cap {
            Some(cap) => cap.try_reserve(requested),
            None => true,
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
        // Default pool does not enable throughput tracking for zero overhead.
        // Callers that want dynamic buffer sizing should chain
        // `.with_throughput_tracking()`.
        Self::new(max_buffers)
    }
}
