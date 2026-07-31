//! Telemetry snapshot and accessor methods for [`BufferPool`].
//!
//! Holds the [`BufferPoolStats`] snapshot type plus the pool's read-only
//! introspection methods (counts, capacities, hit-rate, byte-budget and
//! memory-cap queries). All counters use `Relaxed` ordering since exact
//! cross-counter consistency is not required for telemetry.

use std::sync::atomic::Ordering;

use super::super::allocator::BufferAllocator;
use super::BufferPool;

impl<A: BufferAllocator> BufferPool<A> {
    /// Returns the number of buffers currently in the central queue.
    ///
    /// Does not include the thread-local cached buffer (at most one per
    /// thread). Primarily useful for testing and monitoring. The returned
    /// value is a lock-free snapshot of [`ArrayQueue::len`](crossbeam_queue::ArrayQueue::len)
    /// and may briefly race with concurrent push/pop operations.
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
    pub fn memory_cap(&self) -> Option<usize> {
        self.memory_cap.as_ref().map(|cap| cap.limit())
    }

    /// Returns the configured byte budget for pool retention, or `None`
    /// if no byte budget is set.
    pub fn byte_budget(&self) -> Option<usize> {
        self.byte_budget.as_ref().map(|b| b.limit())
    }

    /// Returns the current bytes retained in the central pool, or `0`
    /// if no byte budget is configured.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.byte_budget.as_ref().map(|b| b.retained()).unwrap_or(0)
    }

    /// Returns the cumulative count of admission rejections due to the
    /// byte budget being full.
    ///
    /// Each rejected admission means a returning buffer was deallocated
    /// rather than retained and a subsequent acquire on an empty pool
    /// will allocate fresh. Always zero when no byte budget is configured.
    #[must_use]
    pub fn total_byte_overflows(&self) -> u64 {
        self.byte_budget
            .as_ref()
            .map(|b| b.overflows())
            .unwrap_or(0)
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
            total_byte_overflows: self.total_byte_overflows(),
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
    /// Number of admission rejections due to the byte budget being full
    /// on return. Each rejection means the returning buffer was
    /// deallocated rather than retained. Zero when no byte budget is set.
    pub total_byte_overflows: u64,
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
