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

    /// Blocks until outstanding memory plus `requested` is within the cap,
    /// then atomically reserves the capacity by incrementing outstanding.
    ///
    /// This combines the wait and checkout into a single atomic operation
    /// under the lock to prevent TOCTOU races where multiple threads pass
    /// the capacity check before either increments outstanding.
    pub(super) fn wait_and_reserve(&self, requested: usize) {
        // Fast path: try to reserve with CAS, no locking needed.
        loop {
            let current = self.outstanding.load(Ordering::Acquire);
            if current + requested > self.limit {
                break; // Fall through to slow path.
            }
            if self
                .outstanding
                .compare_exchange_weak(
                    current,
                    current + requested,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return;
            }
            // CAS failed due to concurrent modification, retry.
        }

        // Slow path: wait on the condvar and reserve under the lock.
        let mut guard = self
            .backpressure
            .lock()
            .expect("backpressure mutex poisoned");
        loop {
            let current = self.outstanding.load(Ordering::Acquire);
            if current + requested <= self.limit {
                // Reserve the capacity while holding the lock.
                self.outstanding
                    .store(current + requested, Ordering::Release);
                return;
            }
            guard = self
                .returned
                .wait(guard)
                .expect("backpressure condvar poisoned");
        }
    }

    /// Tries to atomically reserve `requested` bytes without blocking.
    ///
    /// Returns `true` if the reservation succeeded, `false` if it would
    /// exceed the cap.
    pub(super) fn try_reserve(&self, requested: usize) -> bool {
        loop {
            let current = self.outstanding.load(Ordering::Acquire);
            if current + requested > self.limit {
                return false;
            }
            if self
                .outstanding
                .compare_exchange_weak(
                    current,
                    current + requested,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Records that `size` bytes have been returned and wakes waiters.
    pub(super) fn track_return(&self, size: usize) {
        self.outstanding.fetch_sub(size, Ordering::Release);
        // Wake all blocked acquirers so they can re-check the capacity.
        // With CAS-based reservation in the slow path, only one will
        // succeed at reserving, but all must be woken to re-evaluate.
        self.returned.notify_all();
    }
}
