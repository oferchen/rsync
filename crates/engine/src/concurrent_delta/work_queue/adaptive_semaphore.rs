//! Dynamically resizable counting semaphore for work-queue backpressure.
//!
//! `AdaptiveSemaphore` is a blocking counting semaphore whose capacity can be
//! grown or shrunk at runtime while permits are in flight. It is the
//! backpressure primitive for the dynamic-capacity work queue: a controller
//! observes the [block rate](AdaptiveSemaphore::block_rate_since) and calls
//! [`resize`](AdaptiveSemaphore::resize) to widen the queue when producers stall
//! or narrow it when memory pressure rises.
//!
//! # Design
//!
//! The semaphore is built on a std [`Mutex`] guarding the permit accounting plus
//! a [`Condvar`] that blocked acquirers wait on. No external async runtime is
//! involved, so it composes with the rayon-based consumer threads already used
//! by the work queue.
//!
//! Two invariants hold at all times:
//!
//! - `in_flight` only ever changes by one per [`acquire`](AdaptiveSemaphore::acquire)
//!   / [`try_acquire`](AdaptiveSemaphore::try_acquire) / [`release`](AdaptiveSemaphore::release).
//! - A permit is granted only while `in_flight < cap`; this is re-checked in a
//!   loop after every wakeup, so a spurious wakeup or a concurrent shrink can
//!   never over-issue permits.
//!
//! # Resize semantics
//!
//! Growing the capacity wakes every blocked waiter ([`Condvar::notify_all`]),
//! because more than one slot may have opened. Shrinking the capacity never
//! revokes a permit that is already held: it only lowers the ceiling, so newly
//! released slots are withheld from future acquirers until `in_flight` falls
//! back below the smaller `cap`. This keeps shrinking safe to call while work is
//! in progress - no in-flight task is ever forced to abort.

use std::sync::{Condvar, Mutex, MutexGuard};

/// Smallest capacity an [`AdaptiveSemaphore`] may hold.
///
/// A capacity of zero would deadlock every acquirer, so one is the floor.
pub const MIN_CAPACITY: usize = 1;

/// Largest capacity an [`AdaptiveSemaphore`] may hold.
///
/// This is a sanity ceiling - real work-queue depths are small multiples of the
/// thread count. It guards against an arithmetic mistake in a controller asking
/// for an absurd capacity.
pub const MAX_CAPACITY: usize = 1 << 20;

/// Error returned when a requested capacity falls outside the permitted range.
///
/// Returned by [`AdaptiveSemaphore::new`] and [`AdaptiveSemaphore::resize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeError {
    /// The requested capacity was below [`MIN_CAPACITY`].
    BelowMin {
        /// The capacity that was requested.
        requested: usize,
        /// The enforced minimum ([`MIN_CAPACITY`]).
        min: usize,
    },
    /// The requested capacity was above [`MAX_CAPACITY`].
    AboveMax {
        /// The capacity that was requested.
        requested: usize,
        /// The enforced maximum ([`MAX_CAPACITY`]).
        max: usize,
    },
}

impl std::fmt::Display for ResizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResizeError::BelowMin { requested, min } => {
                write!(f, "capacity {requested} is below the minimum of {min}")
            }
            ResizeError::AboveMax { requested, max } => {
                write!(f, "capacity {requested} is above the maximum of {max}")
            }
        }
    }
}

impl std::error::Error for ResizeError {}

/// Point-in-time snapshot of an [`AdaptiveSemaphore`]'s contention counters.
///
/// Capture a baseline with [`AdaptiveSemaphore::stats`], then pass it back to
/// [`AdaptiveSemaphore::block_rate_since`] to measure how often acquirers had to
/// block over the intervening window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemStats {
    /// Total blocking [`acquire`](AdaptiveSemaphore::acquire) calls observed.
    pub acquires: u64,
    /// Subset of `acquires` that had to wait for a permit to free up.
    pub blocks: u64,
}

/// Mutable interior state guarded by the semaphore's mutex.
struct SemInner {
    /// Current capacity ceiling: the maximum number of simultaneous permits.
    cap: usize,
    /// Permits currently handed out and not yet released.
    in_flight: usize,
    /// Number of [`acquire`](AdaptiveSemaphore::acquire) calls that had to block.
    block_count: u64,
    /// Total number of [`acquire`](AdaptiveSemaphore::acquire) calls.
    acquire_count: u64,
}

/// A counting semaphore whose capacity can change while permits are in flight.
///
/// See the [module documentation](self) for the design and resize semantics.
/// The semaphore is `Send + Sync` and is intended to be shared across threads
/// via `Arc`. Permits are released with [`release`](Self::release); there is no
/// RAII guard, so callers must pair every successful acquire with exactly one
/// release.
pub struct AdaptiveSemaphore {
    inner: Mutex<SemInner>,
    available: Condvar,
}

impl AdaptiveSemaphore {
    /// Creates a semaphore with `initial_cap` permits.
    ///
    /// # Errors
    ///
    /// Returns [`ResizeError`] if `initial_cap` is outside
    /// `[MIN_CAPACITY, MAX_CAPACITY]`.
    pub fn new(initial_cap: usize) -> Result<Self, ResizeError> {
        let cap = validate_cap(initial_cap)?;
        Ok(Self {
            inner: Mutex::new(SemInner {
                cap,
                in_flight: 0,
                block_count: 0,
                acquire_count: 0,
            }),
            available: Condvar::new(),
        })
    }

    /// Acquires one permit, blocking until one is available.
    ///
    /// If no permit is free the call blocks on the internal condition variable
    /// and the per-semaphore `block_count` instrumentation counter is
    /// incremented once for this call. The permit-availability predicate is
    /// re-checked after every wakeup, so spurious wakeups and concurrent
    /// [`resize`](Self::resize) shrinks are handled correctly.
    pub fn acquire(&self) {
        let mut inner = self.lock();
        inner.acquire_count += 1;
        if inner.in_flight >= inner.cap {
            inner.block_count += 1;
            while inner.in_flight >= inner.cap {
                inner = self
                    .available
                    .wait(inner)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
        inner.in_flight += 1;
    }

    /// Attempts to acquire one permit without blocking.
    ///
    /// Returns `true` if a permit was granted, `false` if the semaphore is at
    /// capacity. This call never blocks and is not counted in the blocking
    /// `acquire`/`block` statistics.
    pub fn try_acquire(&self) -> bool {
        let mut inner = self.lock();
        if inner.in_flight < inner.cap {
            inner.in_flight += 1;
            true
        } else {
            false
        }
    }

    /// Releases one permit and wakes a single blocked acquirer.
    ///
    /// The in-flight count is decremented saturatingly, so an unmatched release
    /// (more releases than acquires) cannot underflow - it simply leaves the
    /// count at zero. Exactly one waiter is woken via [`Condvar::notify_one`].
    pub fn release(&self) {
        let mut inner = self.lock();
        inner.in_flight = inner.in_flight.saturating_sub(1);
        drop(inner);
        self.available.notify_one();
    }

    /// Changes the capacity ceiling to `new_cap`.
    ///
    /// Growing wakes every blocked waiter, since a grow can open more than one
    /// slot at once. Shrinking never revokes an in-flight permit: it only lowers
    /// the ceiling, so already-acquired permits stay valid and future acquirers
    /// are withheld until `in_flight` drops below the new, smaller capacity.
    ///
    /// # Errors
    ///
    /// Returns [`ResizeError`] if `new_cap` is outside
    /// `[MIN_CAPACITY, MAX_CAPACITY]`; the capacity is left unchanged.
    pub fn resize(&self, new_cap: usize) -> Result<(), ResizeError> {
        let new_cap = validate_cap(new_cap)?;
        let mut inner = self.lock();
        let grew = new_cap > inner.cap;
        inner.cap = new_cap;
        drop(inner);
        if grew {
            self.available.notify_all();
        }
        Ok(())
    }

    /// Returns the current capacity ceiling.
    #[must_use]
    pub fn current_cap(&self) -> usize {
        self.lock().cap
    }

    /// Returns the number of permits currently in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight
    }

    /// Returns the total number of blocking acquires that had to wait.
    #[must_use]
    pub fn block_count(&self) -> u64 {
        self.lock().block_count
    }

    /// Captures a snapshot of the contention counters.
    ///
    /// Use the returned [`SemStats`] as the baseline argument to
    /// [`block_rate_since`](Self::block_rate_since).
    #[must_use]
    pub fn stats(&self) -> SemStats {
        let inner = self.lock();
        SemStats {
            acquires: inner.acquire_count,
            blocks: inner.block_count,
        }
    }

    /// Returns the fraction of blocking acquires that blocked since `baseline`.
    ///
    /// The result is in `[0.0, 1.0]`: `blocks / acquires` measured over the
    /// window between `baseline` and now. Returns `0.0` when no blocking
    /// acquires occurred in the window, avoiding a divide-by-zero. Counter
    /// differences are computed saturatingly so a stale or mismatched baseline
    /// cannot produce a negative or nonsensical rate.
    #[must_use]
    pub fn block_rate_since(&self, baseline: SemStats) -> f64 {
        let cur = self.stats();
        let acquires = cur.acquires.saturating_sub(baseline.acquires);
        if acquires == 0 {
            return 0.0;
        }
        let blocks = cur.blocks.saturating_sub(baseline.blocks);
        blocks as f64 / acquires as f64
    }

    /// Locks the inner state, recovering the guard if the mutex was poisoned.
    ///
    /// Permit accounting is simple integer arithmetic that cannot leave the
    /// state inconsistent, so a panic in another thread should not permanently
    /// disable the semaphore. Recovering via [`into_inner`](std::sync::PoisonError::into_inner)
    /// keeps it usable.
    fn lock(&self) -> MutexGuard<'_, SemInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for AdaptiveSemaphore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("AdaptiveSemaphore")
            .field("cap", &inner.cap)
            .field("in_flight", &inner.in_flight)
            .field("block_count", &inner.block_count)
            .field("acquire_count", &inner.acquire_count)
            .finish()
    }
}

/// Validates a requested capacity against the permitted range.
fn validate_cap(cap: usize) -> Result<usize, ResizeError> {
    if cap < MIN_CAPACITY {
        Err(ResizeError::BelowMin {
            requested: cap,
            min: MIN_CAPACITY,
        })
    } else if cap > MAX_CAPACITY {
        Err(ResizeError::AboveMax {
            requested: cap,
            max: MAX_CAPACITY,
        })
    } else {
        Ok(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn acquire_release_round_trip() {
        let sem = AdaptiveSemaphore::new(2).unwrap();
        assert!(sem.try_acquire());
        assert!(sem.try_acquire());
        assert_eq!(sem.in_flight(), 2);
        // At capacity now.
        assert!(!sem.try_acquire());
        sem.release();
        assert_eq!(sem.in_flight(), 1);
        // A slot freed up.
        assert!(sem.try_acquire());
        assert_eq!(sem.in_flight(), 2);
    }

    #[test]
    fn try_acquire_when_exhausted() {
        let sem = AdaptiveSemaphore::new(1).unwrap();
        assert!(sem.try_acquire());
        assert!(!sem.try_acquire());
        assert_eq!(sem.in_flight(), 1);
    }

    #[test]
    fn grow_unblocks_waiter() {
        let sem = Arc::new(AdaptiveSemaphore::new(1).unwrap());
        // Main thread holds the only permit.
        assert!(sem.try_acquire());

        let (tx, rx) = mpsc::channel();
        let worker = {
            let sem = Arc::clone(&sem);
            std::thread::spawn(move || {
                // Blocks until the capacity grows (or a release frees a slot).
                sem.acquire();
                tx.send(()).unwrap();
            })
        };

        // The worker cannot have acquired yet - capacity is 1 and it is held.
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());

        // Growing capacity must wake the waiter even though no release occurred.
        sem.resize(2).unwrap();
        rx.recv_timeout(RECV_TIMEOUT)
            .expect("worker should acquire after grow");
        worker.join().unwrap();
        assert_eq!(sem.in_flight(), 2);
    }

    #[test]
    fn shrink_does_not_revoke_in_flight() {
        let sem = AdaptiveSemaphore::new(2).unwrap();
        assert!(sem.try_acquire());
        assert!(sem.try_acquire());
        assert_eq!(sem.in_flight(), 2);

        // Shrink below the in-flight count: existing permits stay valid.
        sem.resize(1).unwrap();
        assert_eq!(sem.current_cap(), 1);
        assert_eq!(sem.in_flight(), 2);

        // No new permit until in_flight drops below the smaller cap.
        sem.release();
        assert_eq!(sem.in_flight(), 1);
        assert!(!sem.try_acquire());

        sem.release();
        assert_eq!(sem.in_flight(), 0);
        assert!(sem.try_acquire());
    }

    #[test]
    fn release_saturates_at_zero() {
        let sem = AdaptiveSemaphore::new(1).unwrap();
        // Release with nothing in flight must not underflow.
        sem.release();
        assert_eq!(sem.in_flight(), 0);
        assert!(sem.try_acquire());
        assert_eq!(sem.in_flight(), 1);
    }

    #[test]
    fn new_rejects_out_of_bounds() {
        assert_eq!(
            AdaptiveSemaphore::new(0).err(),
            Some(ResizeError::BelowMin {
                requested: 0,
                min: MIN_CAPACITY,
            })
        );
        assert_eq!(
            AdaptiveSemaphore::new(MAX_CAPACITY + 1).err(),
            Some(ResizeError::AboveMax {
                requested: MAX_CAPACITY + 1,
                max: MAX_CAPACITY,
            })
        );
    }

    #[test]
    fn resize_rejects_out_of_bounds_and_keeps_cap() {
        let sem = AdaptiveSemaphore::new(4).unwrap();
        assert!(matches!(sem.resize(0), Err(ResizeError::BelowMin { .. })));
        assert!(matches!(
            sem.resize(MAX_CAPACITY + 1),
            Err(ResizeError::AboveMax { .. })
        ));
        // Capacity unchanged after rejected resizes.
        assert_eq!(sem.current_cap(), 4);
        // A valid resize takes effect.
        sem.resize(8).unwrap();
        assert_eq!(sem.current_cap(), 8);
    }

    #[test]
    fn block_count_and_rate_track_waits() {
        let sem = Arc::new(AdaptiveSemaphore::new(1).unwrap());
        let baseline = sem.stats();
        // Hold the single permit so the worker must block.
        sem.acquire();

        let (tx, rx) = mpsc::channel();
        let worker = {
            let sem = Arc::clone(&sem);
            std::thread::spawn(move || {
                sem.acquire();
                tx.send(()).unwrap();
            })
        };

        // Worker is blocked; release lets it proceed.
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        sem.release();
        rx.recv_timeout(RECV_TIMEOUT)
            .expect("worker should acquire after release");
        worker.join().unwrap();

        // Exactly one of the two acquires had to block.
        assert_eq!(sem.block_count(), 1);
        let rate = sem.block_rate_since(baseline);
        assert!((rate - 0.5).abs() < f64::EPSILON, "rate was {rate}");
    }

    #[test]
    fn block_rate_zero_without_acquires() {
        let sem = AdaptiveSemaphore::new(2).unwrap();
        let baseline = sem.stats();
        assert_eq!(sem.block_rate_since(baseline), 0.0);
    }
}
