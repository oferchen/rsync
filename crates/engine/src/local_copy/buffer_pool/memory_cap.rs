//! Hard memory cap with backpressure support for [`BufferPool`](super::BufferPool).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// Hard memory cap with backpressure support.
///
/// When set, the pool tracks total memory outstanding (checked-out buffers)
/// and blocks `acquire` calls that would exceed the cap until a buffer is
/// returned. This prevents unbounded memory growth in high-concurrency
/// scenarios.
#[derive(Debug)]
pub(super) struct MemoryCap {
    /// Maximum allowed bytes across all outstanding and pooled buffers.
    limit: usize,
    /// Current outstanding bytes (buffers checked out by callers, not in pool).
    outstanding: AtomicUsize,
    /// Mutex + condvar pair for blocking when the cap is reached.
    backpressure: Mutex<()>,
    /// Notified when a buffer is returned and outstanding bytes decrease.
    returned: Condvar,
}

impl MemoryCap {
    /// Creates a new memory cap with the given limit in bytes.
    ///
    /// # Panics
    ///
    /// Panics if `max_bytes` is zero.
    pub(super) fn new(max_bytes: usize) -> Self {
        assert!(max_bytes > 0, "memory cap must be greater than zero");
        Self {
            limit: max_bytes,
            outstanding: AtomicUsize::new(0),
            backpressure: Mutex::new(()),
            returned: Condvar::new(),
        }
    }

    /// Returns the configured limit in bytes.
    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the current outstanding (checked-out) bytes.
    pub(super) fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::Relaxed)
    }

    /// Blocks until outstanding memory plus `requested` is within the cap.
    pub(super) fn wait_for_capacity(&self, requested: usize) {
        // Fast path: check without locking.
        if self.outstanding.load(Ordering::Acquire) + requested <= self.limit {
            return;
        }

        // Slow path: wait on the condvar until memory is available.
        let mut guard = self
            .backpressure
            .lock()
            .expect("backpressure mutex poisoned");
        while self.outstanding.load(Ordering::Acquire) + requested > self.limit {
            guard = self
                .returned
                .wait(guard)
                .expect("backpressure condvar poisoned");
        }
    }

    /// Returns `true` if the requested bytes can be allocated without
    /// exceeding the memory cap.
    pub(super) fn try_reserve(&self, requested: usize) -> bool {
        self.outstanding.load(Ordering::Acquire) + requested <= self.limit
    }

    /// Records that `size` bytes have been checked out.
    pub(super) fn track_checkout(&self, size: usize) {
        self.outstanding.fetch_add(size, Ordering::Release);
    }

    /// Records that `size` bytes have been returned and wakes waiters.
    pub(super) fn track_return(&self, size: usize) {
        self.outstanding.fetch_sub(size, Ordering::Release);
        // Wake one blocked acquirer. notify_one is sufficient because
        // each returned buffer can only satisfy one waiter.
        self.returned.notify_one();
    }
}
