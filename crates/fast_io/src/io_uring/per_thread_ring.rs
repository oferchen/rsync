//! Per-thread io_uring ring primitive (IUR-3.a).
//!
//! Foundational lifecycle primitive for the hybrid per-thread topology chosen
//! by IUR-2 (see `docs/design/iur-2-per-thread-rings.md`). Each calling thread
//! gets its own `io_uring::IoUring` instance, lazily constructed on first
//! `with_ring` call and dropped when the thread exits.
//!
//! # Why per-thread
//!
//! The IUR-1 caller-surface audit
//! (`docs/audits/io-uring-shared-ring-audit.md`) and the bench in
//! `crates/fast_io/benches/iouring_per_file_vs_shared.rs` identified three
//! factories - `file_writer`, `file_reader`, `socket_writer` - whose
//! submissions serialise on `SharedRing`'s single submission queue under
//! rayon-parallel callers. A per-thread ring removes that contention because
//! the SQ tail is a non-atomic write to a per-thread cache line and
//! `io_uring::IoUring` is `!Sync` by construction.
//!
//! # When to use this
//!
//! Hot-path factories whose callers fan out across rayon workers or other
//! pinned threads. IUR-3.b/c/d will migrate `file_writer`, `file_reader`,
//! and `socket_writer` to this primitive in follow-up PRs.
//!
//! # When NOT to use this
//!
//! - **One-shot kernel probes** (`linkat::linkat_supported`,
//!   `renameat2::renameat2_supported`, `statx::statx_supported`) run once at
//!   startup and contribute zero hot-path syscalls; per-thread storage would
//!   add a TLS slot per probe site for no measurable gain. They stay on
//!   shared/single rings.
//! - **Disk-commit singleton** (`IoUringDiskBatch`) is already `!Send + !Sync`
//!   and pinned to the disk-commit thread for the life of the session. No
//!   second thread submits to it.
//! - **Re-entrant submit/reap on the same thread.** This primitive enforces
//!   single-borrow via `RefCell`; nested `with_ring` calls return
//!   `io::ErrorKind::WouldBlock` rather than deadlocking or aliasing the SQ
//!   cursor.
//!
//! See IUR-2 design doc section 1.1 for the full hybrid split.
//!
//! # Cleanup semantics
//!
//! The ring lives in `thread_local!` storage. When the OS thread exits the
//! TLS destructor drops the `IoUring`, which `close(2)`s the ring fd and
//! unmaps the SQ/CQ pages. No explicit shutdown is required; rayon workers,
//! `thread::spawn` workers, and the main thread all reach the destructor on
//! normal exit or join. The kernel reclaims any in-flight SQEs on ring-fd
//! close.
//!
//! # Storage choice
//!
//! `RefCell<Option<IoUring>>` rather than `OnceLock<IoUring>` because:
//!
//! - `io_uring::IoUring` is `!Sync` and cannot live behind an `OnceLock`-backed
//!   `Sync` container.
//! - `RefCell::try_borrow_mut` surfaces re-entrant access as a typed error
//!   instead of silently aliasing the SQ cursor (which `OnceLock` would not
//!   prevent).
//!
//! This matches the storage shape used by the shipped
//! `super::session_pool::ThreadLocalRingPool` and the rationale in IUR-2
//! design doc section 2.1.

use std::cell::RefCell;
use std::io;

use io_uring::IoUring;

use super::config::IoUringConfig;

/// Default submission queue depth for per-thread rings.
///
/// 64 entries match the default of [`IoUringConfig::sq_entries`] and the
/// session-wide `IoUringDiskBatch` ring, so bench comparisons between
/// shared-ring and per-thread topologies are like-for-like. See IUR-2
/// design doc section 2.2 for the sizing rationale (32 is too shallow
/// for batched `POLL_ADD + SEND` pairs, 256 inflates per-ring pinned
/// pages without bench evidence the receiver write path queues that
/// deeply).
pub const DEFAULT_RING_DEPTH: u32 = 64;

/// Newtype wrapping the underlying [`io_uring::IoUring`] for the
/// per-thread topology.
///
/// See module documentation for lifecycle, contention model, and
/// cleanup semantics.
pub struct PerThreadRing {
    ring: IoUring,
}

impl PerThreadRing {
    /// Builds a per-thread ring with the default submission queue depth.
    fn new() -> io::Result<Self> {
        let config = IoUringConfig {
            sq_entries: DEFAULT_RING_DEPTH,
            ..IoUringConfig::default()
        };
        let ring = config
            .build_ring()
            .map_err(|e| io::Error::other(format!("per-thread io_uring init failed: {e}")))?;
        Ok(Self { ring })
    }

    /// Returns a mutable reference to the underlying ring.
    fn ring_mut(&mut self) -> &mut IoUring {
        &mut self.ring
    }
}

thread_local! {
    /// The calling thread's per-thread ring, lazily constructed on first
    /// [`with_ring`] use.
    ///
    /// `RefCell<Option<...>>` rather than `OnceCell` because
    /// [`io_uring::IoUring`] is `!Sync` and re-entrant submit/reap must
    /// surface as a typed error instead of aliasing the SQ cursor. See
    /// IUR-2 design doc section 2.1.
    static THREAD_RING: RefCell<Option<PerThreadRing>> = const { RefCell::new(None) };
}

/// Runs `f` against the calling thread's per-thread io_uring instance,
/// constructing the ring on first call.
///
/// The ring lives for the rest of the thread's life; subsequent calls on
/// the same thread re-use it without further `io_uring_setup(2)`
/// syscalls.
///
/// # Errors
///
/// - [`io::ErrorKind::Other`] on `io_uring_setup(2)` failure (kernel
///   rejected the ring; the caller should fall back to per-call rings or
///   standard I/O per IUR-2 section 5.2).
/// - [`io::ErrorKind::WouldBlock`] when the calling thread already holds
///   an outstanding borrow of its per-thread ring (re-entrant
///   submit/reap is not supported; the caller must release the outer
///   borrow before nesting).
/// - Any error returned by `f` itself.
pub fn with_ring<F, R>(f: F) -> io::Result<R>
where
    F: FnOnce(&mut IoUring) -> io::Result<R>,
{
    THREAD_RING.with(|cell| {
        let mut guard = cell.try_borrow_mut().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "per-thread io_uring ring is already borrowed on this thread (re-entrant \
                 submit/reap is not supported; release the outer lease before nesting)",
            )
        })?;
        if guard.is_none() {
            *guard = Some(PerThreadRing::new()?);
        }
        let ring = guard
            .as_mut()
            .expect("per-thread ring populated for the duration of this borrow")
            .ring_mut();
        f(ring)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use io_uring::opcode;

    /// Skip helper for hosts without io_uring (musl CI, seccomp,
    /// kernels < 5.6). Mirrors the pattern in
    /// `session_pool::tests::thread_local_pool_unavailable`.
    fn io_uring_unavailable() -> bool {
        !crate::io_uring::config::is_io_uring_available()
    }

    /// Submits a single `Nop` SQE against the supplied ring, reaps the
    /// matching CQE, and returns the ring fd so the caller can confirm
    /// per-thread isolation.
    fn submit_nop_and_reap(ring: &mut IoUring, op_id: u64) -> io::Result<i32> {
        let entry = opcode::Nop::new().build().user_data(op_id);
        // SAFETY: `Nop` references no caller-owned memory and the SQE is
        // consumed by the kernel only after submit_and_wait().
        unsafe {
            ring.submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }
        ring.submit_and_wait(1)?;
        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("missing CQE after submit_and_wait(1)"))?;
        assert_eq!(cqe.user_data(), op_id, "CQE must match the submitted SQE");
        assert_eq!(cqe.result(), 0, "Nop CQE must report success");
        use std::os::unix::io::AsRawFd;
        Ok(ring.as_raw_fd())
    }

    #[test]
    fn with_ring_lazily_constructs_per_thread_ring() {
        if io_uring_unavailable() {
            eprintln!("skipping per-thread ring test: io_uring unavailable");
            return;
        }
        let fd_first = with_ring(|ring| {
            use std::os::unix::io::AsRawFd;
            Ok(ring.as_raw_fd())
        })
        .expect("first with_ring call builds the ring");
        let fd_second = with_ring(|ring| {
            use std::os::unix::io::AsRawFd;
            Ok(ring.as_raw_fd())
        })
        .expect("second with_ring call re-uses the same ring");
        assert_eq!(
            fd_first, fd_second,
            "consecutive same-thread with_ring calls must hand back the same ring fd"
        );
    }

    #[test]
    fn with_ring_rejects_reentrant_borrow() {
        if io_uring_unavailable() {
            eprintln!("skipping per-thread ring test: io_uring unavailable");
            return;
        }
        let outer = with_ring(|_outer_ring| {
            // Nested call on the same thread must fail loudly, not
            // deadlock or alias the SQ cursor.
            let inner = with_ring(|_inner_ring| Ok(()));
            Ok(inner)
        })
        .expect("outer with_ring call succeeds");
        let inner_err = outer.expect_err("re-entrant with_ring call must error");
        assert_eq!(inner_err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn four_threads_get_independent_rings() {
        if io_uring_unavailable() {
            eprintln!("skipping per-thread ring test: io_uring unavailable");
            return;
        }
        let workers = 4usize;
        let iterations = 1000usize;
        let start_barrier = Arc::new(Barrier::new(workers));
        // Exit barrier keeps all worker rings alive until the parent has
        // collected every fd. Without it, threads can exit between
        // joins, run their thread_local destructors (closing the ring
        // fd), and let the kernel recycle that fd number for the next
        // worker - causing a false "rings collided" assertion.
        let exit_barrier = Arc::new(Barrier::new(workers + 1));
        let unique_op_id = Arc::new(AtomicUsize::new(1));
        let mut handles = Vec::with_capacity(workers);
        for worker in 0..workers {
            let start_barrier = Arc::clone(&start_barrier);
            let exit_barrier = Arc::clone(&exit_barrier);
            let unique_op_id = Arc::clone(&unique_op_id);
            handles.push(thread::spawn(move || -> io::Result<i32> {
                start_barrier.wait();
                // Burn 1000 with_ring calls to confirm there is no
                // hidden lock or panic on the lazy-init / re-acquire
                // path under load.
                for _ in 0..iterations {
                    with_ring(|_ring| Ok(()))?;
                }
                // Submit one Nop with a worker-unique op_id so the
                // CQE round-trip proves each thread's ring is its own
                // SQ/CQ pair: a shared ring would surface a foreign
                // op_id from another worker's earlier submission.
                let op_id =
                    unique_op_id.fetch_add(1, Ordering::Relaxed) as u64 | ((worker as u64) << 32);
                let fd = with_ring(|ring| submit_nop_and_reap(ring, op_id))?;
                // Hold the ring open until the parent has snapshotted
                // every worker's fd. Releasing earlier lets the kernel
                // recycle this fd into another worker's freshly opened
                // ring and the dedup invariant falsely collapses.
                exit_barrier.wait();
                Ok(fd)
            }));
        }
        // Wait until every worker has reached the post-submit barrier;
        // at that point all four rings exist concurrently.
        exit_barrier.wait();
        let mut fds = Vec::with_capacity(workers);
        for handle in handles {
            let fd = handle
                .join()
                .expect("worker did not panic")
                .expect("worker with_ring + Nop round-trip succeeded");
            fds.push(fd);
        }
        // Each worker's ring fd must be distinct: a shared ring would
        // surface the same fd across all workers.
        fds.sort_unstable();
        fds.dedup();
        assert_eq!(
            fds.len(),
            workers,
            "each worker must own a distinct per-thread ring fd"
        );
    }

    #[test]
    fn nop_round_trip_uses_per_thread_ring() {
        if io_uring_unavailable() {
            eprintln!("skipping per-thread ring test: io_uring unavailable");
            return;
        }
        let fd = with_ring(|ring| submit_nop_and_reap(ring, 0xdead_beef))
            .expect("Nop round-trip succeeds on the per-thread ring");
        // The same thread's subsequent with_ring call must hand back the
        // same ring fd we just reaped against.
        let fd_again = with_ring(|ring| {
            use std::os::unix::io::AsRawFd;
            Ok(ring.as_raw_fd())
        })
        .expect("second with_ring call re-uses the per-thread ring");
        assert_eq!(fd, fd_again, "per-thread ring fd is stable for the thread");
    }
}
