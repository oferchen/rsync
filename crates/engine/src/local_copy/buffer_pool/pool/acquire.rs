//! Buffer acquire/return hot path for [`BufferPool`].
//!
//! Implements the two-level buffer lifecycle: acquire (thread-local cache,
//! then lock-free central queue, then fresh allocation), return (thread-local
//! slot, then soft-cap-gated central admission), plus the supporting
//! `admit_or_deallocate` and `pop_buffer` primitives. See the
//! [buffer-pool module documentation](super::super) for design rationale.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::super::adaptive_buffer_size;
use super::super::allocator::BufferAllocator;
use super::super::guard::{BorrowedBufferGuard, BufferGuard};
use super::super::thread_local_cache;
use super::BufferPool;

impl<A: BufferAllocator> BufferPool<A> {
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

    /// Acquires a buffer whose size is driven by the PID controller.
    ///
    /// When a [`buffer controller`](Self::with_buffer_controller) is enabled,
    /// the returned buffer is sized to
    /// [`recommended_buffer_size`](Self::recommended_buffer_size) - the PID
    /// output that tracks the throughput setpoint. When no controller is
    /// present, this falls back to file-size-based adaptive sizing via
    /// [`acquire_adaptive_from`](Self::acquire_adaptive_from).
    ///
    /// This is the preferred acquisition method for the transfer pipeline:
    /// it feeds the controller's recommendation into the actual I/O buffer
    /// size, closing the feedback loop between throughput observation and
    /// buffer allocation.
    ///
    /// The controller recommendation is clamped between `min_size` and
    /// `max_size` (configured at controller build time). If the recommended
    /// size matches the pool's default `buffer_size`, the thread-local cache
    /// and central pool are reused. Otherwise a fresh buffer is allocated at
    /// the recommended size and resized back to the pool default on return.
    #[must_use]
    pub fn acquire_controlled_from(pool: Arc<Self>, file_size: u64) -> BufferGuard<A> {
        let desired = if pool.buffer_controller.is_some() {
            pool.recommended_buffer_size()
        } else {
            adaptive_buffer_size(file_size)
        };

        if desired == pool.buffer_size {
            return Self::acquire_from(pool);
        }

        pool.wait_and_reserve_memory(desired);
        let buffer = pool.allocator.allocate(desired);
        BufferGuard {
            buffer: Some(buffer),
            pool,
        }
    }

    /// Acquires a controller-driven buffer (borrows self).
    ///
    /// Borrowed variant of [`acquire_controlled_from`](Self::acquire_controlled_from).
    /// Returns a guard with a lifetime tied to `self`.
    #[must_use]
    pub fn acquire_controlled(&self, file_size: u64) -> BorrowedBufferGuard<'_, A> {
        let desired = if self.buffer_controller.is_some() {
            self.recommended_buffer_size()
        } else {
            adaptive_buffer_size(file_size)
        };

        if desired == self.buffer_size {
            return self.acquire();
        }

        self.wait_and_reserve_memory(desired);
        let buffer = self.allocator.allocate(desired);
        BorrowedBufferGuard {
            buffer: Some(buffer),
            pool: self,
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
    /// overwrites the buffer via [`Read::read`](std::io::Read::read) before
    /// consuming data (see `transfer.rs` and `parallel_checksum.rs`).
    ///
    /// The return path tries the thread-local cache first (zero sync). If
    /// the slot is occupied, falls through to the lock-free central queue.
    /// If the queue is at capacity (either the soft limit or the underlying
    /// `ArrayQueue` slot count), the buffer is deallocated.
    ///
    /// When a memory cap is configured, outstanding bytes are decremented
    /// and any threads blocked in `acquire` are notified.
    #[allow(unsafe_code)]
    pub(in super::super) fn return_buffer(&self, mut buffer: Vec<u8>) {
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

        // Periodic donation (slab feature only): every Mth return on this
        // thread, pop the slab's oldest buffer and route it to the central
        // overflow queue so idle threads do not pin retention forever.
        #[cfg(feature = "thread-slab-pool")]
        if let Some(donated) = super::super::thread_slab::take_donation() {
            self.admit_or_deallocate(donated, capacity);
        }

        // Release outstanding memory and wake blocked acquirers.
        self.track_return(returned_len);
    }

    /// Admits a buffer to the central queue under the soft cap, or
    /// deallocates it.
    ///
    /// Uses [`compare_exchange_weak`](std::sync::atomic::AtomicUsize::compare_exchange_weak) to
    /// reserve a slot in `central_count` only when the current count is
    /// strictly below `capacity`. On success, the buffer is pushed onto
    /// the lock-free [`ArrayQueue`](crossbeam_queue::ArrayQueue) (always
    /// succeeds because the queue's hard capacity is at least
    /// [`DEFAULT_QUEUE_CAPACITY`](super::DEFAULT_QUEUE_CAPACITY) >= any soft
    /// cap). On rejection (count >= capacity), the buffer is deallocated.
    ///
    /// When a byte budget is configured, the budget reservation runs first
    /// so a rejection short-circuits before any count-slot contention. A
    /// reservation that succeeded but then loses the count-slot race is
    /// released before deallocation so the budget stays accurate.
    fn admit_or_deallocate(&self, buffer: Vec<u8>, capacity: usize) {
        // Byte budget gate (if configured) - reserve bytes before claiming
        // a count slot so the count slot is not held when the byte cap
        // rejects admission. Overflow counter increments inside try_reserve.
        let buffer_bytes = buffer.capacity();
        if let Some(budget) = &self.byte_budget
            && !budget.try_reserve(buffer_bytes)
        {
            self.allocator.deallocate(buffer);
            return;
        }

        let mut current = self.central_count.load(Ordering::Relaxed);
        loop {
            if current >= capacity {
                // Count cap rejected admission - release the byte reservation
                // we made above so it does not permanently shrink the budget.
                if let Some(budget) = &self.byte_budget {
                    budget.release(buffer_bytes);
                }
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
                        if let Some(budget) = &self.byte_budget {
                            budget.release(buffer_bytes);
                        }
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
    /// Uses the lock-free [`ArrayQueue::pop`](crossbeam_queue::ArrayQueue::pop) hot path. The accompanying
    /// `central_count` counter is decremented on success so future returns
    /// can re-admit buffers up to the soft capacity. When adaptive resizing
    /// is enabled, records hit/miss statistics and triggers periodic resize
    /// evaluations (every 64 operations).
    fn pop_buffer(&self) -> Vec<u8> {
        match self.buffers.pop() {
            Some(buffer) => {
                self.central_count.fetch_sub(1, Ordering::Relaxed);
                if let Some(budget) = &self.byte_budget {
                    budget.release(buffer.capacity());
                }
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
}
