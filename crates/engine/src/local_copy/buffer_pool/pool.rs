//! Core [`BufferPool`] implementation.
//!
//! Provides the thread-safe buffer pool backed by a lock-free
//! [`crossbeam_queue::ArrayQueue`] with a thread-local single-slot cache for
//! zero-synchronization acquire/return on the hot path. See the
//! [module-level documentation](super) for design rationale and usage
//! patterns.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crossbeam_queue::ArrayQueue;

use super::allocator::{BufferAllocator, DefaultAllocator};
use super::guard::{BorrowedBufferGuard, BufferGuard};
use super::memory_cap::MemoryCap;
use super::pressure::{PressureTracker, ResizeAction};
use super::thread_local_cache;
use super::throughput::ThroughputTracker;
use super::{COPY_BUFFER_SIZE, adaptive_buffer_size};

/// Default fixed capacity for the lock-free central queue.
///
/// `ArrayQueue` requires a fixed capacity at construction time, so the
/// queue is sized to the larger of the caller-requested `max_buffers`
/// and this default. This headroom allows the adaptive resizer to grow
/// the soft capacity without having to reallocate the queue.
///
/// Matches the upper bound enforced by the adaptive resizer (`MAX_CAPACITY`
/// in `pressure.rs`). At 128 KB per buffer, 64 buffers = 8 MiB of
/// outstanding pooled memory before a fresh allocation is required.
const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Computes the fixed [`ArrayQueue`] capacity for a given soft maximum.
///
/// Returns the larger of `max_buffers` and [`DEFAULT_QUEUE_CAPACITY`], with
/// a floor of `1` because `ArrayQueue::new(0)` panics. Soft-capacity
/// enforcement (including the zero-capacity case) is handled separately
/// in `return_buffer`.
fn queue_capacity(max_buffers: usize) -> usize {
    max_buffers.max(DEFAULT_QUEUE_CAPACITY).max(1)
}

/// A thread-safe pool of reusable I/O buffers with a two-level cache.
///
/// Reduces allocation overhead during file copy operations by maintaining
/// a set of buffers that can be reused across operations. Uses a two-level
/// architecture:
///
/// 1. **Thread-local fast path** - a single-slot `thread_local!` cache per
///    thread provides zero-synchronization acquire/return for the common
///    case where each rayon worker holds one buffer at a time.
///
/// 2. **Central pool** - a lock-free [`ArrayQueue`] stores overflow buffers.
///    Acquire pops from the queue, return pushes back. Both operations
///    are wait-free in the contended case (single CAS) with no syscalls.
///    Only accessed when the thread-local slot misses (empty on acquire,
///    occupied on return).
///
/// # Capacity Enforcement
///
/// The pool has a soft maximum capacity (`max_buffers`). The underlying
/// [`ArrayQueue`] is sized at construction to hold at least
/// [`DEFAULT_QUEUE_CAPACITY`] buffers (or `max_buffers` if larger). The
/// soft capacity is enforced on return via an atomic
/// `compare_exchange_weak` admission counter that allows at most
/// `soft_capacity` concurrent successful admissions, so the central queue
/// never exceeds the soft cap. Thread-local cached buffers do not count
/// against this limit since they are conceptually "in use" by their thread.
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
/// 1. **Acquire** - check thread-local slot, then pop from the lock-free
///    central queue, then allocate fresh.
/// 2. **Use** - caller reads/writes through the RAII guard's `Deref`/`DerefMut`.
/// 3. **Return** - guard's `Drop` impl passes the buffer back via
///    [`return_buffer`](Self::return_buffer), which tries the thread-local
///    slot first, then the central queue.
#[derive(Debug)]
pub struct BufferPool<A: BufferAllocator = DefaultAllocator> {
    /// Central pool of available buffers, backed by a lock-free MPMC queue.
    ///
    /// Only accessed when the thread-local cache misses. Under the typical
    /// rayon workload (one buffer per worker per file), this queue sees
    /// near-zero contention because the thread-local cache absorbs the
    /// hot path. Under heavy concurrency the queue's wait-free push/pop
    /// avoids the syscall overhead of a contended mutex.
    buffers: ArrayQueue<Vec<u8>>,
    /// Number of buffers currently held in the central queue.
    ///
    /// Maintained alongside the [`ArrayQueue`] so the soft-capacity check
    /// on return can be performed via a single `compare_exchange_weak`
    /// rather than a racy `len()` read. Decremented after each successful
    /// pop. Without this counter, multiple concurrent returns could each
    /// observe `len() < capacity` and all push, overshooting the soft cap.
    central_count: AtomicUsize,
    /// Soft maximum number of buffers to retain in the central pool.
    ///
    /// Read atomically on every return to enforce the soft cap and on
    /// every adaptive-resize evaluation. Thread-local cached buffers are
    /// not counted against this limit.
    soft_capacity: AtomicUsize,
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
    /// Optional pressure tracker for adaptive pool resizing.
    ///
    /// When present, tracks hit/miss rates and periodically adjusts the
    /// pool's soft capacity to match demand. Enabled via
    /// [`with_adaptive_resizing`](Self::with_adaptive_resizing).
    pressure: Option<PressureTracker>,
    /// Cumulative count of acquire operations that found a buffer in the
    /// thread-local cache or central pool (no fresh allocation needed).
    ///
    /// Always active regardless of whether adaptive resizing is enabled.
    /// Uses `Relaxed` ordering since exact precision is not required for
    /// telemetry - small counting errors under concurrent access are
    /// acceptable.
    total_hits: AtomicU64,
    /// Cumulative count of acquire operations that required a fresh
    /// allocation because no pooled buffer was available.
    ///
    /// Always active regardless of whether adaptive resizing is enabled.
    total_misses: AtomicU64,
    /// Cumulative count of pool capacity growth events.
    ///
    /// Incremented each time the adaptive resizer increases the soft
    /// capacity. Always zero when adaptive resizing is not enabled.
    total_growths: AtomicU64,
}

impl BufferPool {
    /// Creates a new buffer pool with the specified soft capacity.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation. To supply a custom
    /// allocator, use [`with_allocator`](Self::with_allocator).
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Soft maximum number of buffers to retain in the
    ///   central pool. Thread-local cached buffers are additional.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pool = BufferPool::new(num_cpus::get());
    /// ```
    #[must_use]
    pub fn new(max_buffers: usize) -> Self {
        Self {
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: AtomicUsize::new(max_buffers),
            buffer_size: COPY_BUFFER_SIZE,
            allocator: DefaultAllocator,
            memory_cap: None,
            throughput: None,
            pressure: None,
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_growths: AtomicU64::new(0),
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Soft maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: AtomicUsize::new(max_buffers),
            buffer_size,
            allocator: DefaultAllocator,
            memory_cap: None,
            throughput: None,
            pressure: None,
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_growths: AtomicU64::new(0),
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
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: AtomicUsize::new(max_buffers),
            buffer_size,
            allocator,
            memory_cap: None,
            throughput: None,
            pressure: None,
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_growths: AtomicU64::new(0),
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

    /// Enables adaptive resizing based on allocation pressure.
    ///
    /// When enabled, the pool tracks hit/miss rates using atomic counters
    /// and periodically adjusts its soft capacity:
    ///
    /// - **Grow**: When the miss rate exceeds 20% (too many fresh allocations),
    ///   the capacity is doubled (up to 256).
    /// - **Shrink**: When pool utilization drops below 30% and miss rate is
    ///   low, the capacity is halved (down to 2).
    ///
    /// Pressure evaluation occurs every 64 acquire operations, amortizing
    /// the cost. Between checks, only two `Relaxed` atomic increments are
    /// performed per acquire - negligible overhead on the hot path.
    ///
    /// Adaptive resizing is zero-cost when not enabled.
    #[must_use]
    pub fn with_adaptive_resizing(mut self) -> Self {
        self.pressure = Some(PressureTracker::new());
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
    /// Checks the thread-local cache first (zero synchronization). On miss,
    /// pops from the central pool or allocates fresh. The returned
    /// [`BufferGuard`] automatically returns the buffer to the pool on drop.
    ///
    /// If a memory cap is configured and the outstanding memory equals or
    /// exceeds the cap, this method blocks until a buffer is returned by
    /// another thread (backpressure). Use [`try_acquire_from`](Self::try_acquire_from)
    /// for a non-blocking alternative.
    #[must_use]
    pub fn acquire_from(pool: Arc<Self>) -> BufferGuard<A> {
        // Fast path: check thread-local cache.
        if let Some(buffer) = thread_local_cache::try_take() {
            if buffer.len() == pool.buffer_size {
                pool.total_hits.fetch_add(1, Ordering::Relaxed);
                // Re-reserve memory that was released by return_buffer's track_return.
                pool.wait_and_reserve_memory(pool.buffer_size);
                return BufferGuard {
                    buffer: Some(buffer),
                    pool,
                };
            }
            // Wrong size (from a different pool config) - discard and allocate fresh.
            pool.allocator.deallocate(buffer);
        }

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
        // Fast path: check thread-local cache.
        if let Some(buffer) = thread_local_cache::try_take() {
            if buffer.len() == pool.buffer_size {
                // Re-reserve memory that was released by return_buffer's track_return.
                if !pool.try_reserve_memory(pool.buffer_size) {
                    // Cap reached since we returned - put the buffer back in TLS.
                    if let Some(buf) = thread_local_cache::try_store(buffer) {
                        pool.allocator.deallocate(buf);
                    }
                    return None;
                }
                pool.total_hits.fetch_add(1, Ordering::Relaxed);
                return Some(BufferGuard {
                    buffer: Some(buffer),
                    pool,
                });
            }
            pool.allocator.deallocate(buffer);
        }

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
    /// adaptive size matches the pool's default buffer size, the thread-local
    /// cache and central pool are checked. Otherwise a fresh buffer of the
    /// adaptive size is allocated (it will still be returned to the pool on
    /// drop, where its length is restored to the pool's default).
    #[must_use]
    pub fn acquire_adaptive_from(pool: Arc<Self>, file_size: u64) -> BufferGuard<A> {
        let desired = adaptive_buffer_size(file_size);

        if desired == pool.buffer_size {
            // Fast path: adaptive size matches pool default - check TLS and pool.
            return Self::acquire_from(pool);
        }

        // Slow path: non-standard size - allocate fresh, skip TLS.
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
        // Fast path: check thread-local cache.
        if let Some(buffer) = thread_local_cache::try_take() {
            if buffer.len() == self.buffer_size {
                self.total_hits.fetch_add(1, Ordering::Relaxed);
                // Re-reserve memory that was released by return_buffer's track_return.
                self.wait_and_reserve_memory(self.buffer_size);
                return BorrowedBufferGuard {
                    buffer: Some(buffer),
                    pool: self,
                };
            }
            self.allocator.deallocate(buffer);
        }

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
        // Fast path: check thread-local cache.
        if let Some(buffer) = thread_local_cache::try_take() {
            if buffer.len() == self.buffer_size {
                // Re-reserve memory that was released by return_buffer's track_return.
                if !self.try_reserve_memory(self.buffer_size) {
                    // Cap reached since we returned - put the buffer back in TLS.
                    if let Some(buf) = thread_local_cache::try_store(buffer) {
                        self.allocator.deallocate(buf);
                    }
                    return None;
                }
                self.total_hits.fetch_add(1, Ordering::Relaxed);
                return Some(BorrowedBufferGuard {
                    buffer: Some(buffer),
                    pool: self,
                });
            }
            self.allocator.deallocate(buffer);
        }

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
    /// The return path tries the thread-local cache first (zero sync). If
    /// the slot is occupied, falls through to the lock-free central queue.
    /// If the queue is at capacity (either the soft limit or the underlying
    /// `ArrayQueue` slot count), the buffer is deallocated.
    ///
    /// When a memory cap is configured, outstanding bytes are decremented
    /// and any threads blocked in `acquire` are notified.
    #[allow(unsafe_code)]
    pub(super) fn return_buffer(&self, mut buffer: Vec<u8>) {
        let returned_len = buffer.len();
        let capacity = self.soft_capacity.load(Ordering::Relaxed);

        // Zero-capacity pool: never retain buffers - deallocate immediately.
        if capacity == 0 {
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

        // Fast path: try thread-local cache first (zero synchronization).
        if let Some(buffer) = thread_local_cache::try_store(buffer) {
            // TLS slot occupied - admit to the lock-free central queue.
            // The atomic compare_exchange on `central_count` reserves a
            // slot only if the current count is strictly below the soft
            // capacity; racing returners observe each other's increments
            // so only the first `capacity` admissions succeed. A
            // successful reservation guarantees the subsequent push()
            // succeeds because the queue's hard capacity is sized at or
            // above the maximum soft capacity.
            self.admit_or_deallocate(buffer, capacity);
        }

        // Release outstanding memory and wake blocked acquirers.
        self.track_return(returned_len);
    }

    /// Admits a buffer to the central queue under the soft cap, or
    /// deallocates it.
    ///
    /// Uses [`compare_exchange_weak`](AtomicUsize::compare_exchange_weak) to
    /// reserve a slot in `central_count` only when the current count is
    /// strictly below `capacity`. On success, the buffer is pushed onto
    /// the lock-free [`ArrayQueue`] (always succeeds because the queue's
    /// hard capacity is at least [`DEFAULT_QUEUE_CAPACITY`] >= any soft
    /// cap). On rejection (count >= capacity), the buffer is deallocated.
    fn admit_or_deallocate(&self, buffer: Vec<u8>, capacity: usize) {
        let mut current = self.central_count.load(Ordering::Relaxed);
        loop {
            if current >= capacity {
                self.allocator.deallocate(buffer);
                return;
            }
            match self.central_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Slot reserved - push must succeed because the queue's
                    // hard capacity is >= any value central_count can reach.
                    if let Err(buffer) = self.buffers.push(buffer) {
                        // Defensive fallback: undo the reservation and
                        // deallocate. Unreachable given the queue sizing
                        // invariant in `queue_capacity`.
                        self.central_count.fetch_sub(1, Ordering::Relaxed);
                        self.allocator.deallocate(buffer);
                    }
                    return;
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Pops a buffer from the central queue, or allocates a new one if empty.
    ///
    /// Uses the lock-free [`ArrayQueue::pop`] hot path. The accompanying
    /// `central_count` counter is decremented on success so future returns
    /// can re-admit buffers up to the soft capacity. When adaptive resizing
    /// is enabled, records hit/miss statistics and triggers periodic resize
    /// evaluations (every 64 operations).
    fn pop_buffer(&self) -> Vec<u8> {
        match self.buffers.pop() {
            Some(buffer) => {
                self.central_count.fetch_sub(1, Ordering::Relaxed);
                self.total_hits.fetch_add(1, Ordering::Relaxed);
                if let Some(pressure) = &self.pressure {
                    pressure.record_hit();
                    self.maybe_resize(pressure);
                }
                buffer
            }
            None => {
                self.total_misses.fetch_add(1, Ordering::Relaxed);
                if let Some(pressure) = &self.pressure {
                    pressure.record_miss();
                    self.maybe_resize(pressure);
                }
                self.allocator.allocate(self.buffer_size)
            }
        }
    }

    /// Evaluates pressure statistics and applies resize if warranted.
    ///
    /// Capacity updates are atomic stores; the queue mutations on shrink
    /// are lock-free [`ArrayQueue::pop`] calls. Concurrent acquires may
    /// observe an intermediate state during shrink (a brief window where
    /// the queue still holds buffers above the new soft cap), but the
    /// extras are reclaimed on the next return.
    fn maybe_resize(&self, pressure: &PressureTracker) {
        if !pressure.should_check() {
            return;
        }

        let current_capacity = self.soft_capacity.load(Ordering::Relaxed);
        let available = self.buffers.len();

        match pressure.evaluate(current_capacity, available) {
            ResizeAction::Hold => {}
            ResizeAction::Grow(new_capacity) => {
                self.soft_capacity.store(new_capacity, Ordering::Relaxed);
                self.total_growths.fetch_add(1, Ordering::Relaxed);
            }
            ResizeAction::Shrink(new_capacity) => {
                self.soft_capacity.store(new_capacity, Ordering::Relaxed);
                // Deallocate excess buffers beyond the new capacity.
                while self.buffers.len() > new_capacity {
                    match self.buffers.pop() {
                        Some(buf) => {
                            self.central_count.fetch_sub(1, Ordering::Relaxed);
                            self.allocator.deallocate(buf);
                        }
                        None => break,
                    }
                }
            }
        }
    }

    /// Returns the number of buffers currently in the central queue.
    ///
    /// Does not include the thread-local cached buffer (at most one per
    /// thread). Primarily useful for testing and monitoring. The returned
    /// value is a lock-free snapshot of [`ArrayQueue::len`] and may briefly
    /// race with concurrent push/pop operations.
    #[must_use]
    pub fn available(&self) -> usize {
        self.buffers.len()
    }

    /// Returns the soft maximum number of buffers the central pool will retain.
    ///
    /// Thread-local cached buffers are additional (at most one per thread).
    /// Returns `0` for a zero-capacity pool (never retains buffers).
    ///
    /// When adaptive resizing is enabled, this value may change over time
    /// as the pool adjusts to allocation pressure.
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.soft_capacity.load(Ordering::Relaxed)
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

    /// Returns `true` if adaptive resizing is enabled.
    #[must_use]
    pub fn is_adaptive(&self) -> bool {
        self.pressure.is_some()
    }

    /// Returns the cumulative number of acquire operations that found a
    /// buffer in the thread-local cache or central pool (no fresh
    /// allocation needed).
    #[must_use]
    pub fn total_hits(&self) -> u64 {
        self.total_hits.load(Ordering::Relaxed)
    }

    /// Returns the cumulative number of acquire operations that required
    /// a fresh allocation because no pooled buffer was available.
    #[must_use]
    pub fn total_misses(&self) -> u64 {
        self.total_misses.load(Ordering::Relaxed)
    }

    /// Returns the total number of acquire operations (hits + misses).
    #[must_use]
    pub fn total_acquires(&self) -> u64 {
        self.total_hits() + self.total_misses()
    }

    /// Returns the hit rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no acquires have been recorded yet. The hit rate
    /// measures how effectively the pool reuses buffers - higher values
    /// indicate less allocation overhead.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.total_acquires();
        if total == 0 {
            return 0.0;
        }
        self.total_hits() as f64 / total as f64
    }

    /// Returns the cumulative number of pool capacity growth events.
    ///
    /// Incremented each time adaptive resizing increases the soft capacity.
    /// Always zero when adaptive resizing is not enabled.
    #[must_use]
    pub fn total_growths(&self) -> u64 {
        self.total_growths.load(Ordering::Relaxed)
    }

    /// Returns a snapshot of all telemetry counters.
    ///
    /// The returned [`BufferPoolStats`] captures the current values of all
    /// atomic counters. Because each counter uses `Relaxed` ordering, the
    /// snapshot is not strictly consistent across counters under concurrent
    /// access - individual values are accurate but may reflect slightly
    /// different points in time.
    #[must_use]
    pub fn stats(&self) -> BufferPoolStats {
        BufferPoolStats {
            total_hits: self.total_hits(),
            total_misses: self.total_misses(),
            total_growths: self.total_growths(),
        }
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
        Self::new(max_buffers)
    }
}

impl<A: BufferAllocator> Drop for BufferPool<A> {
    /// Prints a telemetry summary to stderr when the `OC_RSYNC_BUFFER_POOL_STATS`
    /// environment variable is set to `"1"`.
    ///
    /// The env var is checked only at drop time to avoid any overhead during
    /// normal operation.
    fn drop(&mut self) {
        if std::env::var("OC_RSYNC_BUFFER_POOL_STATS").as_deref() == Ok("1") {
            let stats = self.stats();
            eprintln!(
                "BufferPool stats: reuses={} allocations={} growths={} hit_rate={:.1}%",
                stats.total_hits,
                stats.total_misses,
                stats.total_growths,
                self.hit_rate() * 100.0,
            );
        }
    }
}

/// Snapshot of [`BufferPool`] telemetry counters.
///
/// Returned by [`BufferPool::stats`]. All fields are plain integers copied
/// from atomic counters at the time of the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferPoolStats {
    /// Number of acquire operations satisfied from the thread-local cache
    /// or central pool (buffer reuse - no fresh allocation).
    pub total_hits: u64,
    /// Number of acquire operations that required a fresh allocation
    /// because no pooled buffer was available.
    pub total_misses: u64,
    /// Number of times the adaptive resizer increased the pool's soft
    /// capacity. Zero when adaptive resizing is not enabled.
    pub total_growths: u64,
}

impl BufferPoolStats {
    /// Returns the total number of acquire operations (hits + misses).
    #[must_use]
    pub fn total_acquires(&self) -> u64 {
        self.total_hits + self.total_misses
    }

    /// Returns the hit rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no acquires have been recorded.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.total_acquires();
        if total == 0 {
            return 0.0;
        }
        self.total_hits as f64 / total as f64
    }
}
