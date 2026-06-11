//! Pool of long-lived io_uring instances shared across consumers in a session.
//!
//! # Why
//!
//! Today each disk-batch, file-writer, and file-reader builds its own ring via
//! `IoUringConfig::build_ring`. Per-construction the cost is small, but at
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
//!   single relaxed `AtomicUsize` - the contention point at high concurrency
//!   is the per-ring mutex, not the selector.
//!
//! Two pool primitives live in this module:
//!
//! - [`SessionRingPool`] - bounded fleet shared across threads behind per-slot
//!   mutexes. Round-robin selection.
//! - [`ThreadLocalRingPool`] - one ring per OS thread, lazily constructed on
//!   first acquire. Zero locking on the submit path because the ring never
//!   leaves the thread that built it. See
//!   `docs/design/iouring-per-thread-rings.md` (#2243).
//!
//! Consumers pick the primitive that matches their concurrency model. Rayon
//! workers, the disk-commit thread, and any other pinned thread benefit from
//! the thread-local variant. The session-mutex pool remains the right answer
//! when a small ring fleet must be shared across many short-lived sessions.
//!
//! Existing [`crate::io_uring::shared_ring::SharedRing`] consumers stay on
//! their single-owner pattern; migrations land one consumer at a time in
//! follow-up work tracked alongside the designs at
//! `docs/design/iouring-session-ring-pool.md`,
//! `docs/design/iouring-session-ring-pool-impl.md`, and
//! `docs/design/iouring-per-thread-rings.md`.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::thread::ThreadId;

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
    /// `IoUringConfig::build_ring`.
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
    let sqpoll_gated_off =
        config.flags & IORING_SETUP_SQPOLL != 0 && super::config::is_sqpoll_disabled_by_policy();
    if sqpoll_gated_off {
        logging::debug_log!(
            Io,
            1,
            "io_uring session pool: SQPOLL suppressed by --no-io-uring-sqpoll; \
             building a regular ring"
        );
    } else if config.flags & IORING_SETUP_SQPOLL != 0 {
        builder.setup_sqpoll(config.sqpoll_idle_ms);
    }
    builder
        .build(config.entries_per_ring)
        .map_err(|e| std::io::Error::other(format!("io_uring init failed: {e}")))
}

thread_local! {
    /// Per-thread io_uring rings keyed by pool identity.
    ///
    /// Stored as a `Vec<(pool_id, Box<RefCell<...>>)>` rather than a
    /// `HashMap` to keep the access path branch-light: in the common case one
    /// thread interacts with one pool, so the linear scan over a single entry
    /// beats hashing. Each pool's slot is boxed so the inner `RefCell` has a
    /// stable heap address - this lets [`ThreadLocalRingPool::acquire`] hand
    /// the inner cell out across the outer-vector borrow boundary without
    /// risking pointer invalidation when a sibling pool later grows the
    /// vector. The inner `RefCell` enforces single-borrow at runtime; nested
    /// acquires from the same thread fail loudly instead of silently
    /// deadlocking the way a `Mutex` would.
    #[allow(clippy::type_complexity)]
    static THREAD_RINGS: RefCell<Vec<(usize, Box<RefCell<Option<RawIoUring>>>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Process-wide counter for [`ThreadLocalRingPool`] identity.
///
/// Each pool grabs a unique id at construction. Thread-local storage uses the
/// id to disambiguate rings owned by different pools so two pools living on
/// the same thread cannot collide.
static NEXT_POOL_ID: AtomicUsize = AtomicUsize::new(1);

/// Pool that hands each calling thread its own io_uring ring.
///
/// Where [`SessionRingPool`] shares `N` rings across threads behind per-slot
/// mutexes, `ThreadLocalRingPool` lazily allocates one ring per thread the
/// first time that thread calls [`acquire`](Self::acquire). The ring lives in
/// thread-local storage and never crosses thread boundaries, so the submit and
/// reap path holds no lock at all. The trade-off is more pinned kernel pages
/// at high thread fan-in - the design note at
/// `docs/design/iouring-per-thread-rings.md` (#2243) covers the resource
/// accounting and the work-stealing alternative that was rejected on the
/// grounds that stolen SQEs cannot reference the owning ring's registered
/// buffers or fixed-file table.
///
/// # Concurrency model
///
/// - Cloning the pool is cheap (`Arc` of the shared config). Multiple clones
///   share the same per-thread rings on each thread because they share the
///   same pool id.
/// - Two distinct pools sharing a thread each get their own ring slot.
/// - Re-entrant [`acquire`](Self::acquire) calls on the same thread return
///   `None` (the inner [`RefCell`] is already borrowed). This mirrors the
///   `Mutex` poison-style fallback in [`SessionRingPool::acquire`].
pub struct ThreadLocalRingPool {
    id: usize,
    config: Arc<SessionPoolConfig>,
    /// Lower bound on the number of threads that have ever held a lease.
    /// Useful for diagnostics; exposed via [`thread_count`](Self::thread_count).
    thread_count: Arc<AtomicUsize>,
}

impl ThreadLocalRingPool {
    /// Builds a thread-local ring pool with the supplied per-ring config.
    ///
    /// No ring is constructed up front; the first [`acquire`](Self::acquire)
    /// call on each thread builds that thread's ring. This is the right shape
    /// for rayon pools where the worker count is large but only a subset
    /// touches io_uring at any given time.
    #[must_use]
    pub fn new(config: SessionPoolConfig) -> Self {
        Self {
            id: NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed),
            config: Arc::new(config),
            thread_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the per-pool configuration shared with every thread.
    #[must_use]
    pub fn config(&self) -> &SessionPoolConfig {
        &self.config
    }

    /// Returns a lower bound on the number of distinct threads that have ever
    /// successfully leased a ring from this pool.
    ///
    /// This is the count of rings the pool has caused to be constructed; it
    /// only grows. It is the cheapest proxy for "how much kernel state does
    /// this pool pin" without walking every live thread.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.thread_count.load(Ordering::Relaxed)
    }

    /// Returns the id of the calling thread, useful for diagnostics in tests
    /// that want to confirm rings stay pinned to their constructing thread.
    #[must_use]
    pub fn current_thread_id() -> ThreadId {
        thread::current().id()
    }

    /// Leases the calling thread's ring, constructing it on first use.
    ///
    /// Returns `None` when:
    /// - `io_uring_setup(2)` rejects this thread's ring (caller falls back to
    ///   standard I/O).
    /// - The calling thread already holds an outstanding lease from this pool
    ///   (re-entrant submit/reap is not supported).
    pub fn acquire(&self) -> Option<ThreadLocalRingLease<'_>> {
        // Step 1: ensure the slot exists for this pool on this thread, then
        // capture the stable heap address of the inner cell. The outer
        // `THREAD_RINGS` borrow is released before the inner borrow is
        // handed to the lease so unrelated pool acquires on the same thread
        // remain unblocked.
        let raw_cell_ptr: *const RefCell<Option<RawIoUring>> =
            THREAD_RINGS.with(|cell| -> Option<*const RefCell<Option<RawIoUring>>> {
                let mut slots = cell.borrow_mut();
                if let Some(idx) = slots.iter().position(|(pid, _)| *pid == self.id) {
                    return Some(&*slots[idx].1 as *const _);
                }
                slots.push((self.id, Box::new(RefCell::new(None))));
                let idx = slots.len() - 1;
                Some(&*slots[idx].1 as *const _)
            })?;

        // SAFETY: `raw_cell_ptr` references the heap-allocated `RefCell`
        // owned by a `Box` stored inside the thread-local `THREAD_RINGS`
        // vector. The thread-local outlives any code that runs on this
        // thread, the vector only grows (entries are never removed or
        // re-shuffled), and boxing the cell guarantees that future growth of
        // the outer vector cannot move the pointed-to `RefCell`. The cell is
        // `RefCell`, not `&mut`, so concurrent shared reads from elsewhere
        // on this thread remain sound.
        let cell: &RefCell<Option<RawIoUring>> = unsafe { &*raw_cell_ptr };

        let mut guard = cell.try_borrow_mut().ok()?;
        if guard.is_none() {
            let ring = build_ring(&self.config).ok()?;
            *guard = Some(ring);
            self.thread_count.fetch_add(1, Ordering::Relaxed);
        }
        Some(ThreadLocalRingLease { guard })
    }
}

impl Clone for ThreadLocalRingPool {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            config: Arc::clone(&self.config),
            thread_count: Arc::clone(&self.thread_count),
        }
    }
}

/// RAII lease wrapping the calling thread's ring from a
/// [`ThreadLocalRingPool`].
///
/// Holds the per-thread [`RefCell`] borrow. Drop releases the borrow so the
/// same thread can acquire again. The lease is intentionally `!Send`: rings
/// must not migrate between threads (the kernel ring fd is owned by the
/// constructing process, but submission and completion must happen on the
/// same thread to keep SQE/CQE ordering coherent and to avoid sharing the
/// `!Sync` `IoUring` cursor across threads).
pub struct ThreadLocalRingLease<'pool> {
    guard: std::cell::RefMut<'pool, Option<RawIoUring>>,
}

impl<'pool> Deref for ThreadLocalRingLease<'pool> {
    type Target = RawIoUring;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("thread-local ring slot is populated for the duration of a lease")
    }
}

impl<'pool> DerefMut for ThreadLocalRingLease<'pool> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_mut()
            .expect("thread-local ring slot is populated for the duration of a lease")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

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
            let delta = got.abs_diff(expected);
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

    /// Returns true when the host cannot honour `io_uring_setup(2)` so a
    /// `ThreadLocalRingPool` test should bail out gracefully (matches the
    /// pattern at `crates/fast_io/benches/iouring_per_file_vs_shared.rs:97`).
    fn thread_local_pool_unavailable() -> bool {
        !super::super::config::is_io_uring_available()
    }

    #[test]
    fn thread_local_pool_returns_same_ring_id_for_repeated_acquire() {
        if thread_local_pool_unavailable() {
            eprintln!("skipping thread-local pool test: io_uring unavailable");
            return;
        }
        let pool = ThreadLocalRingPool::new(test_config(1));
        let fd_first = {
            let lease = pool.acquire().expect("first acquire builds a ring");
            use std::os::unix::io::AsRawFd;
            lease.as_raw_fd()
        };
        let fd_second = {
            let lease = pool
                .acquire()
                .expect("second acquire on same thread reuses the slot");
            use std::os::unix::io::AsRawFd;
            lease.as_raw_fd()
        };
        assert_eq!(
            fd_first, fd_second,
            "consecutive same-thread acquires must hand back the same ring fd"
        );
        assert_eq!(pool.thread_count(), 1);
    }

    #[test]
    fn thread_local_pool_distinct_threads_get_distinct_rings() {
        if thread_local_pool_unavailable() {
            eprintln!("skipping thread-local pool test: io_uring unavailable");
            return;
        }
        let pool = ThreadLocalRingPool::new(test_config(1));
        let threads = 4usize;
        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                use std::os::unix::io::AsRawFd;
                let lease = pool.acquire().expect("worker can acquire a ring");
                let id = (thread::current().id(), lease.as_raw_fd());
                drop(lease);
                id
            }));
        }
        let mut seen = HashSet::new();
        for handle in handles {
            let (tid, fd) = handle.join().expect("worker did not panic");
            assert!(
                seen.insert((tid, fd)),
                "duplicate (thread, fd) tuple {tid:?} {fd} - rings leaked across threads"
            );
        }
        assert_eq!(
            pool.thread_count(),
            threads,
            "every worker thread must have built exactly one ring"
        );
    }

    #[test]
    fn thread_local_pool_reentrant_acquire_returns_none() {
        if thread_local_pool_unavailable() {
            eprintln!("skipping thread-local pool test: io_uring unavailable");
            return;
        }
        let pool = ThreadLocalRingPool::new(test_config(1));
        let outer = pool.acquire().expect("first lease succeeds");
        let inner = pool.acquire();
        assert!(
            inner.is_none(),
            "nested acquire on same thread must fail rather than deadlock"
        );
        drop(outer);
        // Once the outer lease is released the same thread can acquire again.
        let after = pool.acquire();
        assert!(after.is_some(), "post-drop acquire must succeed");
    }

    #[test]
    fn thread_local_pool_two_pools_keep_separate_slots() {
        if thread_local_pool_unavailable() {
            eprintln!("skipping thread-local pool test: io_uring unavailable");
            return;
        }
        let pool_a = ThreadLocalRingPool::new(test_config(1));
        let pool_b = ThreadLocalRingPool::new(test_config(1));
        use std::os::unix::io::AsRawFd;
        let fd_a = pool_a.acquire().expect("pool A acquire").as_raw_fd();
        let fd_b = pool_b.acquire().expect("pool B acquire").as_raw_fd();
        assert_ne!(
            fd_a, fd_b,
            "two distinct pools on the same thread must own distinct rings"
        );
        assert_eq!(pool_a.thread_count(), 1);
        assert_eq!(pool_b.thread_count(), 1);
    }

    #[test]
    fn thread_local_pool_clone_shares_per_thread_ring() {
        if thread_local_pool_unavailable() {
            eprintln!("skipping thread-local pool test: io_uring unavailable");
            return;
        }
        let pool = ThreadLocalRingPool::new(test_config(1));
        let twin = pool.clone();
        use std::os::unix::io::AsRawFd;
        let fd_original = pool.acquire().expect("original acquire").as_raw_fd();
        let fd_twin = twin.acquire().expect("twin acquire").as_raw_fd();
        assert_eq!(
            fd_original, fd_twin,
            "clones of the same pool must share the per-thread ring"
        );
        assert_eq!(pool.thread_count(), 1);
    }

    #[test]
    fn thread_local_pool_acquire_returns_none_when_setup_rejected() {
        // entries_per_ring = 0 is rejected by io_uring_setup(2). On hosts
        // without io_uring at all the bail-out path also returns None, so the
        // assertion holds in both environments.
        let cfg = SessionPoolConfig {
            ring_count: 1,
            entries_per_ring: 0,
            flags: 0,
            sqpoll_idle_ms: 0,
        };
        let pool = ThreadLocalRingPool::new(cfg);
        assert!(pool.acquire().is_none());
        assert_eq!(pool.thread_count(), 0);
    }
}
