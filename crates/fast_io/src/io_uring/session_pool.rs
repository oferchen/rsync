//! Pool of long-lived io_uring instances shared across consumers in a session.
//!
//! # Why
//!
//! Today each disk-batch, file-writer, and file-reader builds its own ring via
//! [`IoUringConfig::build_ring`]. Per-construction the cost is small, but at
//! the daemon-session level it stacks: bursty connections pay
//! `io_uring_setup(2)` plus optional `IORING_REGISTER_*` calls on every
//! short-lived consumer. The session ring pool amortises that cost by holding
//! a small, fixed fleet of rings that consumers lease for the duration of a
//! single submit/reap cycle.
//!
//! # Shape
//!
//! - [`SessionRingPool`] owns `N` rings behind individual [`std::sync::Mutex`]es.
//! - [`SessionPoolConfig`] picks the fleet size from CPU count (clamped to a
//!   conservative 16 ceiling) and inherits per-ring sizing from the supplied
//!   [`IoUringConfig`].
//! - [`acquire`](SessionRingPool::acquire) returns a [`RingLease`] that
//!   dereferences to the underlying `IoUring`. Selection is round-robin via a
//!   single relaxed [`AtomicUsize`] - the contention point at high concurrency
//!   is the per-ring mutex, not the selector.
//!
//! This module introduces the primitive only. Existing
//! [`crate::io_uring::shared_ring::SharedRing`] consumers stay on their
//! single-mutex pattern; migrations land one consumer at a time in follow-up
//! work tracked alongside the design at
//! `docs/design/iouring-session-ring-pool.md` and
//! `docs/design/iouring-session-ring-pool-impl.md`.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;

use io_uring::IoUring as RawIoUring;

use super::config::IoUringConfig;

/// Highest ring count the pool will allocate by default.
///
/// Matches the recommendation in `docs/design/iouring-session-ring-pool.md`:
/// sixteen is the largest power-of-two fleet that stays under a typical
/// `RLIMIT_NOFILE` of 1024 once each ring claims an io_uring fd plus its
/// registered-file slots.
const DEFAULT_MAX_RINGS: usize = 16;

/// Construction parameters for a [`SessionRingPool`].
///
/// `ring_count` defaults to `min(available_parallelism(), DEFAULT_MAX_RINGS)`
/// and is clamped to at least 1 ring. `entries_per_ring` and `flags` mirror
/// the corresponding fields of [`IoUringConfig`] and the bits accepted by
/// `io_uring_setup(2)` respectively. `flags = 0` builds a regular ring.
#[derive(Debug, Clone)]
pub struct SessionPoolConfig {
    /// Number of rings the pool will allocate up front.
    pub ring_count: usize,
    /// Submission queue depth passed to `io_uring_setup(2)` for every ring.
    pub entries_per_ring: u32,
    /// Setup flags mirror the bits accepted by `io_uring_setup(2)`.
    ///
    /// Zero selects the default regular ring. Recognised bits are translated
    /// to the corresponding [`io_uring::Builder`] setter: bit
    /// `IORING_SETUP_IOPOLL` enables [`setup_iopoll`], bit
    /// `IORING_SETUP_SQPOLL` enables [`setup_sqpoll`] with
    /// [`SessionPoolConfig::sqpoll_idle_ms`]. Unrecognised bits are ignored;
    /// callers that need exotic flags should construct the ring directly via
    /// [`IoUringConfig::build_ring`](crate::io_uring::IoUringConfig::build_ring).
    ///
    /// [`setup_iopoll`]: io_uring::Builder::setup_iopoll
    /// [`setup_sqpoll`]: io_uring::Builder::setup_sqpoll
    pub flags: u32,
    /// Idle timeout (milliseconds) for the SQPOLL kernel thread when
    /// `flags` requests SQPOLL. Mirrors
    /// [`IoUringConfig::sqpoll_idle_ms`].
    pub sqpoll_idle_ms: u32,
}

impl Default for SessionPoolConfig {
    fn default() -> Self {
        Self::from_io_uring_config(&IoUringConfig::default())
    }
}

impl SessionPoolConfig {
    /// Derives a pool config from the per-ring [`IoUringConfig`].
    ///
    /// The ring count comes from the host's available parallelism, clamped to
    /// `[1, DEFAULT_MAX_RINGS]`. The submission queue depth is copied from
    /// `config.sq_entries`. Setup flags default to zero; callers that need
    /// SQPOLL or other flags should set [`SessionPoolConfig::flags`] after
    /// construction.
    #[must_use]
    pub fn from_io_uring_config(config: &IoUringConfig) -> Self {
        let detected = thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        let ring_count = detected.clamp(1, DEFAULT_MAX_RINGS);
        Self {
            ring_count,
            entries_per_ring: config.sq_entries,
            flags: 0,
            sqpoll_idle_ms: config.sqpoll_idle_ms,
        }
    }

    /// Returns a config with `ring_count` overridden.
    ///
    /// A zero or `None`-like input is clamped to 1 so the pool always owns at
    /// least one ring.
    #[must_use]
    pub fn with_ring_count(mut self, ring_count: usize) -> Self {
        self.ring_count = ring_count.max(1);
        self
    }
}

/// Pool of long-lived io_uring rings shared across consumers in a session.
///
/// The pool eagerly constructs all rings at [`new`](Self::new) /
/// [`try_new`](Self::try_new) time. Consumers call [`acquire`](Self::acquire)
/// to obtain a [`RingLease`] that derefs to a `&mut IoUring` for the duration
/// of one submit/reap cycle. The lease holds the ring's mutex; releasing the
/// lease (drop) frees the ring for the next acquirer.
///
/// Round-robin selection uses a single `AtomicUsize` incremented with relaxed
/// ordering. The kernel ring fds and any registered resources live for the
/// lifetime of the pool; [`Drop`] closes each ring deterministically.
pub struct SessionRingPool {
    rings: Vec<Mutex<RawIoUring>>,
    next: AtomicUsize,
    config: SessionPoolConfig,
}

impl SessionRingPool {
    /// Builds a pool of `config.ring_count` rings.
    ///
    /// Returns the first construction error and drops any partially built
    /// rings so the caller never sees a half-initialised pool. Use
    /// [`try_new`](Self::try_new) if the caller prefers `Option`-style
    /// failure handling.
    pub fn new(config: SessionPoolConfig) -> std::io::Result<Self> {
        let count = config.ring_count.max(1);
        let mut rings = Vec::with_capacity(count);
        for _ in 0..count {
            let ring = build_ring(&config)?;
            rings.push(Mutex::new(ring));
        }
        Ok(Self {
            rings,
            next: AtomicUsize::new(0),
            config,
        })
    }

    /// Builds a pool, returning `None` on any construction failure.
    ///
    /// Convenience wrapper for call sites that already fall back to per-object
    /// rings when io_uring is unavailable.
    #[must_use]
    pub fn try_new(config: SessionPoolConfig) -> Option<Self> {
        Self::new(config).ok()
    }

    /// Returns the number of rings owned by the pool.
    #[must_use]
    pub fn ring_count(&self) -> usize {
        self.rings.len()
    }

    /// Returns the per-pool configuration used to build the rings.
    #[must_use]
    pub fn config(&self) -> &SessionPoolConfig {
        &self.config
    }

    /// Leases the next ring in round-robin order, blocking until it is free.
    ///
    /// Returns `None` only when the pool was constructed empty - which the
    /// constructors prevent by clamping `ring_count` to at least 1. The mutex
    /// poison case is mapped to `None` so callers can fall back to a private
    /// ring rather than panic.
    #[must_use]
    pub fn acquire(&self) -> Option<RingLease<'_>> {
        if self.rings.is_empty() {
            return None;
        }
        let slot = self.next.fetch_add(1, Ordering::Relaxed) % self.rings.len();
        let guard = self.rings[slot].lock().ok()?;
        Some(RingLease { slot, guard })
    }

    /// Leases the ring at the given slot, blocking until it is free.
    ///
    /// Returns `None` when `slot` is out of bounds or the mutex is poisoned.
    /// Exposed for tests and for deterministic-affinity callers; production
    /// consumers should use [`acquire`](Self::acquire) for fair round-robin
    /// distribution.
    #[must_use]
    pub fn acquire_slot(&self, slot: usize) -> Option<RingLease<'_>> {
        let guard = self.rings.get(slot)?.lock().ok()?;
        Some(RingLease { slot, guard })
    }
}

impl Drop for SessionRingPool {
    fn drop(&mut self) {
        // Explicitly drop the rings to make the order observable to readers.
        // The kernel ring fd, SQPOLL kthread (if any), and registered
        // resources are released in `RawIoUring::drop`. Clearing the vector
        // here ties that release to pool drop rather than relying on field
        // declaration order.
        self.rings.clear();
    }
}

/// RAII handle to one leased ring from a [`SessionRingPool`].
///
/// Holds the ring's mutex for its entire lifetime. Drop releases the mutex
/// so the next [`SessionRingPool::acquire`] call can claim the slot.
pub struct RingLease<'pool> {
    slot: usize,
    guard: MutexGuard<'pool, RawIoUring>,
}

impl<'pool> RingLease<'pool> {
    /// Returns the slot index of the leased ring.
    ///
    /// Useful for tests and tracing; production code should not branch on the
    /// slot index because round-robin selection is intentionally opaque.
    #[must_use]
    pub fn slot(&self) -> usize {
        self.slot
    }
}

impl<'pool> Deref for RingLease<'pool> {
    type Target = RawIoUring;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'pool> DerefMut for RingLease<'pool> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

/// `IORING_SETUP_IOPOLL` from `include/uapi/linux/io_uring.h`.
const IORING_SETUP_IOPOLL: u32 = 1 << 0;
/// `IORING_SETUP_SQPOLL` from `include/uapi/linux/io_uring.h`.
const IORING_SETUP_SQPOLL: u32 = 1 << 1;

fn build_ring(config: &SessionPoolConfig) -> std::io::Result<RawIoUring> {
    let mut builder = io_uring::IoUring::builder();
    if config.flags & IORING_SETUP_IOPOLL != 0 {
        builder.setup_iopoll();
    }
    if config.flags & IORING_SETUP_SQPOLL != 0 {
        builder.setup_sqpoll(config.sqpoll_idle_ms);
    }
    builder
        .build(config.entries_per_ring)
        .map_err(|e| std::io::Error::other(format!("io_uring init failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::Barrier;

    fn test_config(rings: usize) -> SessionPoolConfig {
        SessionPoolConfig {
            ring_count: rings,
            entries_per_ring: 8,
            flags: 0,
            sqpoll_idle_ms: 0,
        }
    }

    fn skip_if_no_io_uring(result: &std::io::Result<SessionRingPool>) -> bool {
        if let Err(err) = result {
            eprintln!("skipping session-pool test: io_uring unavailable: {err}");
            return true;
        }
        false
    }

    #[test]
    fn default_config_clamps_ring_count_to_ceiling() {
        let cfg = SessionPoolConfig::default();
        assert!(cfg.ring_count >= 1);
        assert!(cfg.ring_count <= DEFAULT_MAX_RINGS);
        assert_eq!(cfg.entries_per_ring, IoUringConfig::default().sq_entries);
        assert_eq!(cfg.flags, 0);
    }

    #[test]
    fn with_ring_count_clamps_zero_to_one() {
        let cfg = SessionPoolConfig::default().with_ring_count(0);
        assert_eq!(cfg.ring_count, 1);
    }

    #[test]
    fn try_new_returns_none_on_failure() {
        // A SQ depth of zero is rejected by io_uring_setup(2); the pool must
        // surface that as None rather than panicking.
        let cfg = SessionPoolConfig {
            ring_count: 1,
            entries_per_ring: 0,
            flags: 0,
            sqpoll_idle_ms: 0,
        };
        assert!(SessionRingPool::try_new(cfg).is_none());
    }

    #[test]
    fn acquire_four_leases_visits_distinct_rings() {
        let pool_result = SessionRingPool::new(test_config(4));
        if skip_if_no_io_uring(&pool_result) {
            return;
        }
        let pool = pool_result.expect("pool builds when io_uring is available");
        assert_eq!(pool.ring_count(), 4);

        let mut leases = Vec::with_capacity(4);
        let mut slots = HashSet::new();
        for _ in 0..4 {
            let lease = pool.acquire().expect("ring lease available");
            slots.insert(lease.slot());
            leases.push(lease);
        }
        assert_eq!(slots.len(), 4, "four leases must cover four distinct rings");
    }

    #[test]
    fn concurrent_acquires_balance_within_quarter() {
        let pool_result = SessionRingPool::new(test_config(4));
        if skip_if_no_io_uring(&pool_result) {
            return;
        }
        let pool = Arc::new(pool_result.expect("pool builds"));
        let threads = 8usize;
        let per_thread = 64usize;
        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::with_capacity(threads);
        let counts = Arc::new(
            (0..pool.ring_count())
                .map(|_| AtomicUsize::new(0))
                .collect::<Vec<_>>(),
        );

        for _ in 0..threads {
            let pool = Arc::clone(&pool);
            let counts = Arc::clone(&counts);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..per_thread {
                    let lease = pool.acquire().expect("lease available");
                    counts[lease.slot()].fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread did not panic");
        }

        let total = threads * per_thread;
        let expected = total / pool.ring_count();
        let tolerance = expected.div_ceil(4); // within 25% of the mean
        for (idx, counter) in counts.iter().enumerate() {
            let got = counter.load(Ordering::Relaxed);
            let delta = if got > expected {
                got - expected
            } else {
                expected - got
            };
            assert!(
                delta <= tolerance,
                "ring {idx} took {got} leases, expected ~{expected} (tolerance {tolerance})"
            );
        }
    }

    #[test]
    fn drop_releases_all_rings() {
        let pool_result = SessionRingPool::new(test_config(2));
        if skip_if_no_io_uring(&pool_result) {
            return;
        }
        let pool = pool_result.expect("pool builds");
        assert_eq!(pool.ring_count(), 2);
        drop(pool);
        // Re-create immediately. If the previous pool leaked an fd or kthread
        // this second construction would still succeed under normal limits,
        // but exercising the drop->rebuild path here keeps the regression
        // observable when run under `nextest --test-threads=1` with strict
        // RLIMIT_NOFILE.
        let second = SessionRingPool::new(test_config(2));
        if skip_if_no_io_uring(&second) {
            return;
        }
        assert_eq!(second.expect("second pool builds").ring_count(), 2);
    }

    #[test]
    fn acquire_slot_returns_none_for_out_of_bounds() {
        let pool_result = SessionRingPool::new(test_config(2));
        if skip_if_no_io_uring(&pool_result) {
            return;
        }
        let pool = pool_result.expect("pool builds");
        assert!(pool.acquire_slot(2).is_none());
        assert!(pool.acquire_slot(usize::MAX).is_none());
    }
}
