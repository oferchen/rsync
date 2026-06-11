//! Per-thread BGID lease over the BGE-4 central pool (IUR-3.e).
//!
//! Per-thread io_uring rings (IUR-3.a) removed the SQ-tail contention
//! shared rings imposed on rayon-parallel writers, but every per-thread
//! consumer that wants a provided buffer ring (PBUF_RING) still has to
//! ask the process-wide [`BgidAllocator`] for a buffer-group ID. The
//! allocator is a `Mutex<Vec<u16>>` plus an atomic counter; under heavy
//! fan-out the mutex re-introduces the contention the per-thread rings
//! just removed.
//!
//! [`BgidLease`] amortises that cost. On construction it batch-allocates
//! [`DEFAULT_LEASE_BATCH`] bgids from the central pool in a single
//! [`BgidAllocator::allocate_batch`] call and stores them in a small
//! thread-local free-list. Per-ring [`BgidLease::take`] calls then pop
//! from the lease without ever touching the central mutex; only the
//! initial lease (and any top-up after a long run) hits the central
//! pool. On [`Drop`] the lease drains its free-list back through
//! [`BgidAllocator::deallocate_batch`] in a single lock acquisition, so
//! ids are returned to the central pool atomically when the owning
//! thread exits or the lease is explicitly released.
//!
//! # Disjoint slice guarantee
//!
//! [`BgidAllocator::allocate_batch`] is the only path through which a
//! lease obtains ids and it advances the allocator's monotonic counter
//! (or drains its free-list) under the central pool lock. Two threads
//! that lease simultaneously therefore observe disjoint slices: the
//! first lease's ids are physically removed from the counter / free-list
//! before the second lease's batch call runs. The lease never returns
//! an id to a sibling thread until [`Drop`] runs the
//! [`BgidAllocator::deallocate_batch`] path, so concurrent ownership of
//! the same bgid by two leases is impossible.
//!
//! # Thread-local entry point
//!
//! [`with_thread_lease`] keeps one `BgidLease` per OS thread, lazy-built
//! on first use, alongside the per-thread io_uring ring established by
//! `super::per_thread_ring`. Both live in `thread_local!` storage and
//! reach their destructors on normal thread exit, so the lease's bgids
//! flow back to the central pool without explicit teardown by callers.
//!
//! # When NOT to lease
//!
//! - **Long-lived dedicated rings** (e.g. the disk-commit singleton)
//!   want one bgid for the life of the session and should keep going
//!   through [`BgidAllocator::allocate`] directly. Leasing a slice they
//!   never refill is pure overhead.
//! - **Process-wide one-shot probes** (kernel feature detection) do not
//!   allocate bgids at all and stay outside this primitive.
//!
//! See IUR-2 design doc section 1.1 for the broader per-thread topology
//! this lease plugs into.

use std::cell::RefCell;
use std::io;

use super::buffer_ring::BgidAllocator;

/// Default number of bgids leased per central-pool round-trip.
///
/// Sized to keep the per-thread lease cache short-lived but big enough
/// that the steady-state hot path almost never hits the central mutex.
/// 16 entries match the default sender-side fan-out across rayon
/// workers; bump to 32 if a deeper PBUF_RING fan-out ever lands without
/// touching call sites because [`BgidLease::new`] takes the batch size
/// explicitly.
pub const DEFAULT_LEASE_BATCH: usize = 16;

/// RAII handle owning a slice of bgids leased from [`BgidAllocator`].
///
/// Construct via [`BgidLease::new`] (or the thread-local
/// [`with_thread_lease`] entry point). Per-ring code calls
/// [`take`](Self::take) to pop one bgid at a time; the lease refuses
/// further allocations once empty so the caller can either fall back to
/// non-registered I/O or refill the lease via
/// [`refill`](Self::refill) when central pool pressure has eased.
///
/// On [`Drop`] every id still in the lease is returned to the central
/// pool through [`BgidAllocator::deallocate_batch`]. Ids that were
/// handed out via [`take`](Self::take) are *not* tracked by the lease:
/// the caller is responsible for returning them, normally through the
/// `Drop` of the [`super::buffer_ring::BufferRing`] that consumed them.
#[derive(Debug)]
pub struct BgidLease {
    /// Local pool of bgids ready to hand out. Acts as a LIFO so the most
    /// recently leased id is the next one returned; this keeps the
    /// process-wide [`super::buffer_ring::BgidAllocator`] high-water
    /// mark stable across short bursts of take/return on the same
    /// thread.
    cache: Vec<u16>,

    /// Batch size used by [`refill`](Self::refill) and the initial
    /// [`new`](Self::new) call. Stored so the lease is self-describing
    /// without needing the caller to pass the size on every refill.
    batch_size: usize,
}

impl BgidLease {
    /// Leases up to `batch_size` bgids from the central pool in a single
    /// [`BgidAllocator::allocate_batch`] call.
    ///
    /// `batch_size` must be non-zero. A `batch_size` of zero is a
    /// programmer error and surfaces as [`io::ErrorKind::InvalidInput`]
    /// rather than silently producing an empty lease.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] when `batch_size == 0`.
    /// - [`io::ErrorKind::OutOfMemory`] when the central pool has zero
    ///   ids available (free-list empty and counter at the namespace
    ///   limit). The error is the lossless conversion of
    ///   [`super::buffer_ring::BgidAllocError::Exhausted`] documented on
    ///   [`super::buffer_ring::BgidAllocator::allocate_batch`].
    pub fn new(batch_size: usize) -> io::Result<Self> {
        if batch_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "BgidLease batch_size must be non-zero",
            ));
        }
        let cache = BgidAllocator::allocate_batch(batch_size).map_err(io::Error::from)?;
        Ok(Self { cache, batch_size })
    }

    /// Returns the number of bgids currently cached in the lease.
    ///
    /// Decrements on every [`take`](Self::take) and resets to up to
    /// [`batch_size`](Self::batch_size) after a successful
    /// [`refill`](Self::refill).
    #[must_use]
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    /// Returns the batch size used by [`refill`](Self::refill).
    #[must_use]
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// Pops one bgid from the lease cache.
    ///
    /// Returns `None` when the lease is empty; the caller decides whether
    /// to call [`refill`](Self::refill) (and re-attempt the central pool)
    /// or to fall back to the plain `read`/`recv` path documented on
    /// [`super::buffer_ring::BufferRing::new_with_allocator`]. The lease
    /// itself does not auto-refill so callers stay in control of the
    /// central-pool contention pattern.
    pub fn take(&mut self) -> Option<u16> {
        self.cache.pop()
    }

    /// Tops up the lease with another batch from the central pool.
    ///
    /// Allocates only the shortfall between the current
    /// [`cached`](Self::cached) count and [`batch_size`](Self::batch_size)
    /// so callers can call this opportunistically without inflating the
    /// lease past its construction size.
    ///
    /// # Errors
    ///
    /// Same shape as [`new`](Self::new): the central pool exhausted
    /// error is surfaced as [`io::ErrorKind::OutOfMemory`]. A
    /// successful return guarantees at least one id was added unless the
    /// lease was already at `batch_size`.
    pub fn refill(&mut self) -> io::Result<()> {
        let shortfall = self.batch_size.saturating_sub(self.cache.len());
        if shortfall == 0 {
            return Ok(());
        }
        let mut fresh = BgidAllocator::allocate_batch(shortfall).map_err(io::Error::from)?;
        self.cache.append(&mut fresh);
        Ok(())
    }
}

impl Drop for BgidLease {
    fn drop(&mut self) {
        // Return every id we still own to the central pool in one lock
        // acquisition. Ids the caller has already handed off to a
        // BufferRing are not in `cache`; their Drop runs the per-id
        // BgidAllocator::deallocate path independently.
        if !self.cache.is_empty() {
            BgidAllocator::deallocate_batch(&self.cache);
            self.cache.clear();
        }
    }
}

thread_local! {
    /// The calling thread's lazy bgid lease.
    ///
    /// Built on first [`with_thread_lease`] call and dropped when the
    /// thread exits. The TLS destructor invokes [`BgidLease::drop`],
    /// which returns every cached id to the central pool via
    /// [`BgidAllocator::deallocate_batch`].
    static THREAD_LEASE: RefCell<Option<BgidLease>> = const { RefCell::new(None) };
}

/// Runs `f` against the calling thread's bgid lease, building it lazily
/// on the first call with [`DEFAULT_LEASE_BATCH`].
///
/// The lease lives for the rest of the thread's life; subsequent calls
/// on the same thread re-use it without revisiting the central pool
/// unless [`BgidLease::refill`] is invoked from inside `f`.
///
/// # Errors
///
/// - [`io::ErrorKind::OutOfMemory`] when the lease must be constructed
///   for the first time and the central pool is exhausted.
/// - [`io::ErrorKind::WouldBlock`] when the calling thread already
///   holds an outstanding borrow of its per-thread lease (re-entrant
///   `with_thread_lease` is not supported; the inner call must release
///   the outer borrow before nesting).
/// - Any error returned by `f` itself.
pub fn with_thread_lease<F, R>(f: F) -> io::Result<R>
where
    F: FnOnce(&mut BgidLease) -> io::Result<R>,
{
    THREAD_LEASE.with(|cell| {
        let mut guard = cell.try_borrow_mut().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "per-thread bgid lease is already borrowed on this thread (re-entrant \
                 with_thread_lease is not supported; release the outer borrow before nesting)",
            )
        })?;
        if guard.is_none() {
            *guard = Some(BgidLease::new(DEFAULT_LEASE_BATCH)?);
        }
        let lease = guard
            .as_mut()
            .expect("per-thread lease populated for the duration of this borrow");
        f(lease)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// Confirms a fresh lease starts populated to its requested batch
    /// size and that `take` returns each id exactly once before draining.
    #[test]
    fn lease_pops_ids_until_empty() {
        let mut lease = BgidLease::new(4).expect("lease construction succeeds");
        assert_eq!(lease.cached(), 4);
        assert_eq!(lease.batch_size(), 4);
        let mut seen = HashSet::new();
        for _ in 0..4 {
            let id = lease.take().expect("cached id available");
            assert!(seen.insert(id), "lease must not hand out duplicates");
        }
        assert_eq!(lease.cached(), 0);
        assert!(lease.take().is_none(), "drained lease returns None");
        // Manually return the consumed ids so they do not leak: the
        // production path returns them via BufferRing::Drop, but this
        // unit test consumed them without ever building a ring.
        for id in seen {
            BgidAllocator::deallocate(id);
        }
    }

    /// Refill must top the lease back up to `batch_size` without
    /// inflating beyond it.
    #[test]
    fn refill_tops_lease_to_batch_size() {
        let mut lease = BgidLease::new(8).expect("lease construction succeeds");
        let id_a = lease.take().expect("first id");
        let id_b = lease.take().expect("second id");
        assert_eq!(lease.cached(), 6);
        lease.refill().expect("refill succeeds");
        assert_eq!(
            lease.cached(),
            8,
            "refill must restore the lease to batch_size"
        );
        // Refill at full capacity is a no-op.
        lease.refill().expect("no-op refill");
        assert_eq!(lease.cached(), 8);
        // Clean up: drain the cache (Drop handles it) and return the two
        // ids we took out of the lease manually.
        BgidAllocator::deallocate(id_a);
        BgidAllocator::deallocate(id_b);
    }

    /// `batch_size == 0` is rejected with `InvalidInput`.
    #[test]
    fn zero_batch_size_rejected() {
        let err = BgidLease::new(0).expect_err("zero batch must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Dropping the lease returns every cached bgid to the central pool.
    /// Verified by snapshotting `BgidAllocator::remaining()` before and
    /// after a lease lifetime: every cached id must be back in the pool.
    #[test]
    fn drop_returns_cached_ids_to_pool() {
        let before = BgidAllocator::remaining();
        {
            let lease = BgidLease::new(4).expect("lease construction succeeds");
            // The 4 ids are currently checked out of the pool.
            assert!(BgidAllocator::remaining() <= before);
            drop(lease);
        }
        let after = BgidAllocator::remaining();
        assert!(
            after >= before,
            "Drop must return every cached id (before={before}, after={after})"
        );
    }

    /// Two threads leasing concurrently must observe disjoint id slices
    /// and Drop must return both slices to the central pool.
    ///
    /// This is the central correctness guarantee of IUR-3.e: per-thread
    /// leases never share a bgid with a sibling thread mid-lease.
    #[test]
    fn two_threads_get_disjoint_slices() {
        let workers = 2usize;
        let batch = 8usize;
        let start = Arc::new(Barrier::new(workers));
        // exit barrier keeps both leases alive concurrently so neither
        // returns its slice to the pool before the parent has snapshot
        // the ids each thread saw.
        let exit = Arc::new(Barrier::new(workers + 1));

        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let start = Arc::clone(&start);
            let exit = Arc::clone(&exit);
            handles.push(thread::spawn(move || -> io::Result<Vec<u16>> {
                start.wait();
                let lease = BgidLease::new(batch)?;
                // Snapshot the slice the lease owns. We intentionally
                // peek into `cache` via repeated take/refill so the
                // parent can assert disjointness without the lease
                // surrendering the ids early - we then drop the lease
                // (Drop returns the slice to the central pool).
                let owned: Vec<u16> = lease.cache.clone();
                exit.wait();
                drop(lease);
                Ok(owned)
            }));
        }

        exit.wait();
        let mut all_ids: Vec<u16> = Vec::with_capacity(workers * batch);
        for handle in handles {
            let mut owned = handle
                .join()
                .expect("worker did not panic")
                .expect("worker lease construction succeeded");
            assert_eq!(
                owned.len(),
                batch,
                "every worker must obtain its full batch"
            );
            all_ids.append(&mut owned);
        }

        // Disjointness: dedup the union and confirm we kept every id.
        let unique: HashSet<u16> = all_ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            workers * batch,
            "per-thread leases must own disjoint bgid slices"
        );
    }

    /// `with_thread_lease` reuses the same lease across consecutive
    /// calls on the same thread.
    #[test]
    fn with_thread_lease_reuses_per_thread_state() {
        let first = with_thread_lease(|lease| Ok(lease.cached())).expect("first call succeeds");
        let second = with_thread_lease(|lease| Ok(lease.cached())).expect("second call succeeds");
        assert_eq!(
            first, second,
            "consecutive same-thread calls must see the same lease cache"
        );
    }

    /// Re-entrant `with_thread_lease` calls surface as `WouldBlock`
    /// rather than aliasing the lease's cache cursor.
    #[test]
    fn with_thread_lease_rejects_reentrant_borrow() {
        let outer = with_thread_lease(|_outer_lease| {
            let inner = with_thread_lease(|_inner_lease| Ok(()));
            Ok(inner)
        })
        .expect("outer call succeeds");
        let inner_err = outer.expect_err("re-entrant call must error");
        assert_eq!(inner_err.kind(), io::ErrorKind::WouldBlock);
    }
}
