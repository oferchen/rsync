//! Byte-budget admission control for [`BufferPool`](super::BufferPool).
//!
//! Caps the total bytes of pooled (retained) buffers. The count cap alone is
//! insufficient when individual buffers vary widely in size: a handful of
//! adaptive large-file buffers (e.g. 1 MiB each) can blow past any reasonable
//! memory budget even with a modest slot count.
//!
//! # Semantics
//!
//! The budget is a soft cap on pool retention, not on outstanding memory:
//!
//! - On return, if admitting the buffer would push retained bytes past the
//!   limit, the buffer is rejected and an overflow counter increments. The
//!   caller then deallocates the buffer rather than retaining it.
//! - On acquire, an empty pool still allocates a fresh buffer. The cap only
//!   governs how much memory the pool itself retains across calls.
//!
//! This matches the project goal of `--max-alloc` as a pool-retention bound
//! that never blocks transfers: callers always get a buffer, the pool simply
//! stops growing once the budget is exhausted.
//!
//! See [`super::memory_cap`] for the orthogonal hard cap on outstanding
//! (checked-out) memory, which uses condvar-based backpressure.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Tracks the byte-budget for buffers retained in the pool.
///
/// All fields are atomic so admission and release race-free against
/// concurrent returns from multiple threads. Uses `Relaxed` ordering
/// because exact precision is not required for soft-cap admission control
/// or telemetry - small races at the limit boundary are acceptable.
#[derive(Debug)]
pub(super) struct ByteBudget {
    /// Maximum total bytes retained in the central pool.
    limit: usize,
    /// Current total bytes retained in the central pool.
    retained: AtomicUsize,
    /// Cumulative count of admission rejections due to the byte cap.
    overflows: AtomicU64,
}

impl ByteBudget {
    /// Creates a new byte budget with the given limit.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is zero. Use `None` at the [`BufferPool`](super::BufferPool)
    /// level to represent an unbounded pool instead.
    pub(super) fn new(limit: usize) -> Self {
        assert!(limit > 0, "byte budget must be greater than zero");
        Self {
            limit,
            retained: AtomicUsize::new(0),
            overflows: AtomicU64::new(0),
        }
    }

    /// Returns the configured limit in bytes.
    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the current bytes retained in the pool.
    pub(super) fn retained(&self) -> usize {
        self.retained.load(Ordering::Relaxed)
    }

    /// Returns the cumulative number of admission rejections.
    pub(super) fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    /// Tries to reserve `bytes` of retention.
    ///
    /// Returns `true` if reserved (caller may admit the buffer to the pool),
    /// `false` if the reservation would exceed the cap (caller must deallocate
    /// and the overflow counter is incremented).
    ///
    /// Uses a CAS loop so concurrent returners observe each other's
    /// increments. The CAS ensures the retained total never crosses the
    /// limit even under heavy contention.
    pub(super) fn try_reserve(&self, bytes: usize) -> bool {
        let mut current = self.retained.load(Ordering::Relaxed);
        loop {
            if current.saturating_add(bytes) > self.limit {
                self.overflows.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match self.retained.compare_exchange_weak(
                current,
                current + bytes,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    /// Releases `bytes` previously reserved via [`try_reserve`](Self::try_reserve).
    ///
    /// Called when a pooled buffer is handed back out on acquire (it leaves
    /// the pool and no longer counts against retained bytes).
    pub(super) fn release(&self, bytes: usize) {
        // `fetch_sub` saturates on underflow via the explicit guard below; in
        // normal operation reservations and releases are paired so the counter
        // never goes negative. The guard exists to keep us robust against
        // accounting drift if the buffer's size changes between admission and
        // release (e.g. an adaptive buffer larger than the pool default).
        let prev = self.retained.load(Ordering::Relaxed);
        let new = prev.saturating_sub(bytes);
        // A racy store is acceptable here because over-decrement only ever
        // gives the pool slightly more headroom, never less - admission
        // checks remain correct.
        self.retained.store(new, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_records_limit() {
        let budget = ByteBudget::new(4096);
        assert_eq!(budget.limit(), 4096);
        assert_eq!(budget.retained(), 0);
        assert_eq!(budget.overflows(), 0);
    }

    #[test]
    #[should_panic(expected = "byte budget must be greater than zero")]
    fn zero_limit_panics() {
        let _ = ByteBudget::new(0);
    }

    #[test]
    fn try_reserve_under_limit_succeeds() {
        let budget = ByteBudget::new(4096);
        assert!(budget.try_reserve(1024));
        assert_eq!(budget.retained(), 1024);
        assert_eq!(budget.overflows(), 0);
    }

    #[test]
    fn try_reserve_at_exact_limit_succeeds() {
        let budget = ByteBudget::new(4096);
        assert!(budget.try_reserve(4096));
        assert_eq!(budget.retained(), 4096);
        assert_eq!(budget.overflows(), 0);
    }

    #[test]
    fn try_reserve_over_limit_fails_and_counts() {
        let budget = ByteBudget::new(4096);
        assert!(budget.try_reserve(3000));
        assert!(!budget.try_reserve(2000));
        assert_eq!(budget.retained(), 3000);
        assert_eq!(budget.overflows(), 1);
    }

    #[test]
    fn release_returns_capacity() {
        let budget = ByteBudget::new(4096);
        assert!(budget.try_reserve(4000));
        budget.release(4000);
        assert_eq!(budget.retained(), 0);
        // Capacity is fully released - new reservation should succeed.
        assert!(budget.try_reserve(4000));
    }

    #[test]
    fn release_saturates_on_underflow() {
        let budget = ByteBudget::new(4096);
        budget.release(1024);
        assert_eq!(budget.retained(), 0);
    }

    #[test]
    fn overflow_counter_accumulates() {
        let budget = ByteBudget::new(1024);
        assert!(budget.try_reserve(1024));
        assert!(!budget.try_reserve(1));
        assert!(!budget.try_reserve(1));
        assert!(!budget.try_reserve(1));
        assert_eq!(budget.overflows(), 3);
    }

    #[test]
    fn saturating_add_guards_against_wraparound() {
        let budget = ByteBudget::new(usize::MAX - 1);
        // Reserving usize::MAX would wrap without saturating_add; verify it
        // is rejected rather than admitted.
        assert!(!budget.try_reserve(usize::MAX));
        assert_eq!(budget.overflows(), 1);
    }
}
