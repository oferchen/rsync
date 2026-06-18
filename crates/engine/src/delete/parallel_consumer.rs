//! Feature-gated parallel `DeleteEmitter` consumer (DEL-2.c).
//!
//! This module wires the DEL-2.a [`super::ReorderBuffer`] and the DEL-2.b
//! [`super::CohortBatcher`] into a producer/consumer pipeline that drains
//! sealed cohorts under a dedicated consumer thread while dispatching each
//! cohort's [`super::DeleteOperation`] entries on the rayon thread pool.
//!
//! # Scope
//!
//! - The module is gated behind the `parallel-delete-consumer` Cargo
//!   feature so production builds remain byte-for-byte unchanged. DEL-2.d
//!   migrates the receiver-side call site that today routes through
//!   [`super::emitter::DeleteEmitter::emit_all`]. This change is additive
//!   only: with the feature off, the symbols here are not compiled and the
//!   existing sequential emitter remains the only consumer.
//! - The consumer preserves the DEL-1.a strict cross-cohort wire ordering
//!   for `NDX_DEL_STATS` accumulation: cohort `N + 1` cannot begin
//!   dispatch until every op in cohort `N` has completed. Within a
//!   cohort, ops dispatch in parallel because each op targets a distinct
//!   destination leaf inside the same parent directory and the destination
//!   filesystem reconciles ref counts (DEL-1.a section 5.1).
//! - Per DEL-1.b section 6.1 a producer-side panic surfaces through
//!   [`super::CohortBatcher::record_panic`] / [`super::CohortBatcher::is_panicked`].
//!   The consumer observes the latch between cohorts and bails before
//!   dispatching subsequent cohorts so the wire image matches what the
//!   sequential emitter would have produced up to the panic point.
//!
//! # Wake-up model
//!
//! Producers call [`ParallelDeleteEmitter::enqueue_cohort`] to publish a
//! sealed cohort. The call notifies a `Condvar` so the consumer's parked
//! loop wakes and pulls one [`super::CohortBatch`] at a time via
//! [`super::CohortBatcher::drain_batch`]. The predicate
//! `head_is_ready || producers_done || is_panicked` follows the DEL-1.b
//! section 3.2 / DEL-1.c section 6 wake-up rules: the consumer parks on
//! an empty batcher and resumes the moment a producer either seals the
//! head cohort, declares end-of-stream via [`ParallelDeleteEmitter::mark_producers_done`],
//! or latches a panic.
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
//!   (`delete_item`): per-cohort dispatch order the consumer preserves.
//! - `target/interop/upstream-src/rsync-3.4.1/main.c:225-247`
//!   (`write_del_stats` / `read_del_stats`): the goodbye-phase frame the
//!   wire ordering invariant protects. The consumer never emits the frame
//!   itself; it preserves cohort identity so the unchanged generator-side
//!   writer ships the correct totals.

use std::io;
use std::sync::{Arc, Condvar, Mutex};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use super::cohort_batcher::{CohortBatch, CohortBatcher};
use super::emitter::{DeleteFs, EmitterErrorPolicy};
use super::plan::DeleteEntryKind;
use super::reorder_buffer::{DeleteCohortKey, DeleteOperation, ReorderBufferError};
use protocol::DeleteStats;

/// Outcome returned by [`ParallelDeleteEmitter::run`].
///
/// Bundles the consumed [`DeleteFs`] dispatcher, the accumulated stats,
/// the `io_error` bitmask, and the mapped exit code so callers can both
/// surface the exit code and inspect the dispatcher's recorded trace in
/// tests. Mirrors the shape of [`super::DrainOutcome`] used by the
/// sequential emitter so DEL-2.d's call-site migration is mechanical.
#[derive(Debug)]
pub struct ParallelDrainOutcome<F: DeleteFs> {
    /// Filesystem dispatcher consumed by the parallel run. Production
    /// callers drop this; tests inspect [`super::RecordingDeleteFs::events`]
    /// to verify per-cohort dispatch order.
    pub fs: F,
    /// Per-kind deletion totals folded across every drained cohort.
    pub stats: DeleteStats,
    /// Accumulated non-fatal I/O error bitmask. Non-zero means at least
    /// one dispatch returned a non-fatal `io::Error` under the active
    /// [`EmitterErrorPolicy`].
    pub io_error: i32,
    /// Mapped exit code (`0`, `23`, or `24`).
    pub exit_code: i32,
}

/// Cohort-coordinated parallel `DeleteEmitter` consumer.
///
/// The emitter owns the shared `SharedBatcher` (a `Mutex<CohortBatcher>`
/// paired with a `Condvar`) plus the configuration the consumer thread
/// needs to dispatch each [`DeleteOperation`] through the configured
/// [`DeleteFs`]. The producer-facing API is intentionally minimal -
/// [`Self::enqueue_cohort`] and [`Self::mark_producers_done`] - so DEL-2.d
/// can wire the receiver-side traversal driver in without exposing the
/// internal Condvar machinery.
///
/// # Concurrency model
///
/// - **Producers** call [`Self::enqueue_cohort`] on any rayon worker. Each
///   call takes the shared mutex briefly to publish a sealed cohort and
///   signal the consumer's Condvar. With DEL-2.b's single-call sealing
///   the producer never holds the lock across a `DeleteFs` syscall.
/// - **Consumer** runs inside [`Self::run`] which spawns a dedicated OS
///   thread (not a rayon worker - the consumer loops on a Condvar and
///   would otherwise pin a worker, starving the producer side). The
///   thread parks until the predicate
///   `head_is_ready || producers_done || is_panicked` fires, then
///   dispatches one [`CohortBatch`] at a time. Each cohort drains in
///   strict rank order; within a cohort the ops dispatch via
///   `rayon::iter::ParallelIterator::par_iter` so distinct destination
///   leaves run concurrently.
///
/// # Wire-ordering invariant
///
/// The consumer fully drains cohort `N` (parallel dispatch + stat fold +
/// error accumulation) before pulling cohort `N + 1` from the batcher.
/// This satisfies DEL-1.a section 5.2's "stats accumulation must be
/// complete before the frame is written" requirement: the consumer
/// returns the folded [`DeleteStats`] only after every per-cohort fold
/// has executed, so the unchanged goodbye writer at
/// `crates/transfer/src/generator/transfer/goodbye.rs` serialises the
/// correct totals when DEL-2.d wires the receiver through this path.
pub struct ParallelDeleteEmitter<F: DeleteFs> {
    fs: F,
    policy: EmitterErrorPolicy,
    shared: Arc<SharedBatcher>,
}

/// Mutex-protected [`CohortBatcher`] paired with a [`Condvar`] for the
/// DEL-1.b producer/consumer wake-up loop.
///
/// The shape mirrors the DEL-1.b section 3.3 decision to use
/// `Mutex<()>` + `Condvar` over a lock-free queue: keyed in-order drain
/// is the load-bearing requirement and a `Condvar` predicate is the
/// natural fit. The lock is taken only for state transitions
/// (insert/seal in [`super::CohortBatcher::enqueue_cohort`], drain in
/// [`super::CohortBatcher::drain_batch`]), never across `DeleteFs`
/// syscalls.
#[derive(Debug, Default)]
struct SharedBatcher {
    batcher: Mutex<CohortBatcher>,
    /// Latched once [`ParallelDeleteEmitter::mark_producers_done`] is
    /// called so the consumer can exit cleanly when the batcher is empty.
    producers_done: Mutex<bool>,
    /// Signals state changes to the parked consumer loop.
    cond: Condvar,
}

impl<F: DeleteFs + Sync + Send + 'static> ParallelDeleteEmitter<F> {
    /// Constructs a parallel emitter with the default
    /// [`EmitterErrorPolicy`] (continue on non-fatal errors).
    #[must_use]
    pub fn new(fs: F) -> Self {
        Self::with_policy(fs, EmitterErrorPolicy::default())
    }

    /// Constructs a parallel emitter with a caller-supplied policy.
    #[must_use]
    pub fn with_policy(fs: F, policy: EmitterErrorPolicy) -> Self {
        Self {
            fs,
            policy,
            shared: Arc::new(SharedBatcher::default()),
        }
    }

    /// Publishes one sealed cohort for the consumer to drain.
    ///
    /// Producers call this once per cohort (one rayon task per
    /// destination parent directory per DEL-1.c section 3.1). The call
    /// takes the shared mutex briefly to enqueue and signals the
    /// consumer's Condvar so a parked consumer wakes immediately.
    ///
    /// # Errors
    ///
    /// Surfaces [`ReorderBufferError::BufferFull`] when adding a new
    /// cohort would exceed [`super::MAX_BUFFERED_COHORTS`], and
    /// [`ReorderBufferError::RankConflict`] when `key` is already
    /// buffered under a different rank.
    pub fn enqueue_cohort(
        &self,
        key: DeleteCohortKey,
        rank: u64,
        ops: Vec<DeleteOperation>,
    ) -> Result<(), ReorderBufferError> {
        let result = {
            let mut batcher = lock_or_recover(&self.shared.batcher);
            batcher.enqueue_cohort(key, rank, ops)
        };
        self.shared.cond.notify_one();
        result
    }

    /// Latches the producer-side panic flag so the consumer bails at the
    /// next cohort boundary.
    ///
    /// Per DEL-1.c section 6, a producer that unwinds mid-cohort
    /// publishes an empty cohort and records this flag. The consumer
    /// checks the latch between cohorts and stops before dispatching any
    /// subsequent cohort, matching the "lose cohorts after the panic on
    /// the wire" semantics from DEL-1.b section 6.1.
    pub fn record_panic(&self) {
        {
            let batcher = lock_or_recover(&self.shared.batcher);
            batcher.record_panic();
        }
        self.shared.cond.notify_one();
    }

    /// Signals that no more cohorts will be enqueued.
    ///
    /// The consumer drains any remaining sealed cohorts and exits cleanly.
    /// Without this signal a consumer with an empty batcher would park
    /// forever on the Condvar.
    pub fn mark_producers_done(&self) {
        {
            let mut done = lock_or_recover(&self.shared.producers_done);
            *done = true;
        }
        self.shared.cond.notify_all();
    }

    /// Spawns the consumer thread and drains the batcher to completion.
    ///
    /// Must be called from a context that has already published every
    /// cohort the consumer should drain (or that will publish them on a
    /// separate producer thread before calling [`Self::mark_producers_done`]).
    /// The call consumes `self` so the underlying [`DeleteFs`] can be
    /// surfaced in the returned [`ParallelDrainOutcome`].
    ///
    /// # Concurrency
    ///
    /// The consumer runs on a freshly spawned OS thread to avoid pinning
    /// a rayon worker. Inside that thread each [`CohortBatch`] entry's
    /// ops dispatch in parallel via `rayon::par_iter`; cohorts themselves
    /// drain strictly serially to preserve the DEL-1.a cross-cohort
    /// wire-ordering invariant.
    ///
    /// # Errors
    ///
    /// Surfaces a fatal [`io::Error`] when the [`EmitterErrorPolicy`]
    /// classifies a dispatch failure as fatal (mirrors
    /// [`super::emitter::DeleteEmitter::emit_all`] behaviour). Non-fatal
    /// errors accumulate into the returned outcome's `io_error` bitmask
    /// and the drain continues.
    pub fn run(self) -> io::Result<ParallelDrainOutcome<F>> {
        let Self { fs, policy, shared } = self;
        let fs = Arc::new(fs);
        let consumer_fs = Arc::clone(&fs);
        let consumer_shared = Arc::clone(&shared);
        let consumer_handle = std::thread::Builder::new()
            .name("oc-rsync-del-consumer".into())
            .spawn(move || consumer_loop(consumer_shared, consumer_fs, policy))
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to spawn parallel delete consumer thread: {err}"
                ))
            })?;
        let summary = consumer_handle
            .join()
            .map_err(|_| io::Error::other("parallel delete consumer thread panicked"))??;
        let fs = Arc::try_unwrap(fs).map_err(|_| {
            io::Error::other(
                "parallel delete consumer leaked a DeleteFs Arc; consumer thread did not release its reference",
            )
        })?;
        let exit_code = map_exit_code(summary.io_error);
        Ok(ParallelDrainOutcome {
            fs,
            stats: summary.stats,
            io_error: summary.io_error,
            exit_code,
        })
    }
}

impl<F: DeleteFs + std::fmt::Debug> std::fmt::Debug for ParallelDeleteEmitter<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelDeleteEmitter")
            .field("fs", &self.fs)
            .field("policy", &self.policy)
            .finish()
    }
}

#[derive(Debug, Default)]
struct ConsumerSummary {
    stats: DeleteStats,
    io_error: i32,
}

fn consumer_loop<F: DeleteFs + Sync + Send>(
    shared: Arc<SharedBatcher>,
    fs: Arc<F>,
    policy: EmitterErrorPolicy,
) -> io::Result<ConsumerSummary> {
    let mut summary = ConsumerSummary::default();
    loop {
        let batch = match wait_for_batch(&shared)? {
            Some(batch) => batch,
            None => return Ok(summary),
        };
        for entry in batch.into_entries() {
            if lock_or_recover(&shared.batcher).is_panicked() {
                return Ok(summary);
            }
            dispatch_cohort(&fs, &policy, &entry.ops, &mut summary)?;
        }
    }
}

/// Parks the consumer until the head cohort is sealed, all producers are
/// done, or a panic latches. Returns `Ok(Some(batch))` on a ready batch
/// (possibly empty if the panic latch fired between wake-up and drain)
/// and `Ok(None)` when the producers signalled end-of-stream and no
/// cohorts remain.
fn wait_for_batch(shared: &Arc<SharedBatcher>) -> io::Result<Option<CohortBatch>> {
    let mut batcher = lock_or_recover(&shared.batcher);
    loop {
        if batcher.is_panicked() {
            return Ok(None);
        }
        if batcher.head_is_ready() {
            return Ok(Some(batcher.drain_batch()));
        }
        let producers_done = *lock_or_recover(&shared.producers_done);
        if producers_done && batcher.is_empty() {
            return Ok(None);
        }
        batcher = shared
            .cond
            .wait(batcher)
            .map_err(|err| io::Error::other(err.to_string()))?;
    }
}

/// Dispatches one cohort's ops in parallel on rayon. Each op runs the
/// path-based [`DeleteFs`] method matching its kind; per-op results fold
/// back into `summary` after the parallel section joins.
fn dispatch_cohort<F: DeleteFs + Sync + Send>(
    fs: &Arc<F>,
    policy: &EmitterErrorPolicy,
    ops: &[DeleteOperation],
    summary: &mut ConsumerSummary,
) -> io::Result<()> {
    if ops.is_empty() {
        return Ok(());
    }
    let results: Vec<io::Result<DeleteEntryKind>> = ops
        .par_iter()
        .map(|op| dispatch_one(fs.as_ref(), op))
        .collect();
    for result in results {
        match result {
            Ok(kind) => increment_stat(&mut summary.stats, kind),
            Err(err) => {
                if is_fatal_error(&err) {
                    return Err(err);
                }
                accumulate_nonfatal(&mut summary.io_error, policy, &err);
                if !policy.continue_on_error {
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}

fn dispatch_one<F: DeleteFs>(fs: &F, op: &DeleteOperation) -> io::Result<DeleteEntryKind> {
    let outcome = match op.kind {
        DeleteEntryKind::File => fs.unlink_file(&op.path),
        DeleteEntryKind::Dir => dispatch_dir(fs, &op.path),
        DeleteEntryKind::Symlink => fs.unlink_symlink(&op.path),
        DeleteEntryKind::Device => fs.unlink_device(&op.path),
        DeleteEntryKind::Special => fs.unlink_special(&op.path),
    };
    outcome.map(|()| op.kind)
}

fn dispatch_dir<F: DeleteFs>(fs: &F, path: &std::path::Path) -> io::Result<()> {
    match fs.rmdir(path) {
        Ok(()) => Ok(()),
        Err(err) if is_not_empty(&err) => fs.remove_dir_all(path),
        Err(err) => Err(err),
    }
}

fn is_not_empty(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::DirectoryNotEmpty)
}

fn is_fatal_error(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::PermissionDenied)
}

/// Mirrors [`super::emitter::policy`] `IOERR_GENERAL` (bit 0) so the
/// caller's exit-code mapping matches the sequential emitter.
const IOERR_GENERAL: i32 = 1;
/// Mirrors [`super::emitter::policy`] `IOERR_VANISHED_ONLY` (bit 1).
const IOERR_VANISHED_ONLY: i32 = 1 << 1;

fn accumulate_nonfatal(io_error: &mut i32, policy: &EmitterErrorPolicy, err: &io::Error) {
    if policy.ignore_errors {
        return;
    }
    if err.kind() == io::ErrorKind::NotFound {
        *io_error |= IOERR_VANISHED_ONLY;
    } else {
        *io_error |= IOERR_GENERAL;
    }
}

fn map_exit_code(io_error: i32) -> i32 {
    if io_error == 0 {
        0
    } else if io_error == IOERR_VANISHED_ONLY {
        super::emitter::EMITTER_VANISHED_EXIT_CODE
    } else {
        super::emitter::EMITTER_PARTIAL_EXIT_CODE
    }
}

fn increment_stat(stats: &mut DeleteStats, kind: DeleteEntryKind) {
    match kind {
        DeleteEntryKind::File => stats.files = stats.files.saturating_add(1),
        DeleteEntryKind::Dir => stats.dirs = stats.dirs.saturating_add(1),
        DeleteEntryKind::Symlink => stats.symlinks = stats.symlinks.saturating_add(1),
        DeleteEntryKind::Device => stats.devices = stats.devices.saturating_add(1),
        DeleteEntryKind::Special => stats.specials = stats.specials.saturating_add(1),
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poison) => poison.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::delete::emitter::RecordingDeleteFs;

    fn key(path: &str) -> DeleteCohortKey {
        DeleteCohortKey::new(PathBuf::from(path))
    }

    fn op(name: &str) -> DeleteOperation {
        DeleteOperation::new(
            PathBuf::from(format!("/dst/{name}")),
            OsString::from(name),
            DeleteEntryKind::File,
        )
    }

    /// DEL-2.c empty-batcher path: with no cohorts published and
    /// `mark_producers_done` invoked before `run`, the consumer must
    /// drain cleanly and surface an empty outcome instead of parking
    /// forever on the Condvar.
    #[test]
    fn empty_batcher_drains_cleanly() {
        let emitter = ParallelDeleteEmitter::new(RecordingDeleteFs::new());
        emitter.mark_producers_done();
        let outcome = emitter.run().expect("empty drain returns Ok");
        assert!(
            outcome.fs.events().is_empty(),
            "no dispatches expected when no cohorts were enqueued"
        );
        assert_eq!(outcome.stats.files, 0);
        assert_eq!(outcome.io_error, 0);
        assert_eq!(outcome.exit_code, 0);
    }

    /// DEL-2.c single-cohort path: a sealed cohort with three ops
    /// dispatches every op exactly once through `DeleteFs`. The recorded
    /// event log proves all three syscalls landed; the per-op stats fold
    /// into the cohort's bucket counters.
    #[test]
    fn single_sealed_cohort_dispatches_all_ops() {
        let emitter = ParallelDeleteEmitter::new(RecordingDeleteFs::new());
        let ops = vec![op("a"), op("b"), op("c")];
        emitter.enqueue_cohort(key("dir0"), 0, ops).unwrap();
        emitter.mark_producers_done();
        let outcome = emitter.run().expect("drain returns Ok");
        let mut paths: Vec<PathBuf> = outcome.fs.events().into_iter().map(|e| e.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/dst/a"),
                PathBuf::from("/dst/b"),
                PathBuf::from("/dst/c"),
            ]
        );
        assert_eq!(outcome.stats.files, 3);
        assert_eq!(outcome.io_error, 0);
        assert_eq!(outcome.exit_code, 0);
    }

    /// DEL-1.a cross-cohort wire-ordering invariant: cohort `N + 1`'s
    /// dispatch must not begin until every op in cohort `N` has
    /// completed. The test instruments `DeleteFs` to record an ordered
    /// timeline of `(cohort_id, "start"|"finish")` events as each op
    /// enters and leaves the dispatch. With a small sleep inside each
    /// op the rayon pool has time to interleave cohorts if cross-cohort
    /// serialisation is missing; the assertion then proves every
    /// cohort-0 finish precedes every cohort-1 start. The 5 s join
    /// timeout catches the alternative failure mode where a broken
    /// consumer deadlocks instead of interleaving.
    #[test]
    fn cross_cohort_ordering_is_strict() {
        // The leaf name's first character carries the cohort id ('0' or
        // '1'); the recorded timeline pairs the id with a "start"/"finish"
        // tag so the assertion below can prove every cohort-0 finish
        // happens before every cohort-1 start.
        #[derive(Debug)]
        struct OrderingFs {
            timeline: Arc<Mutex<Vec<(u8, &'static str)>>>,
        }
        impl DeleteFs for OrderingFs {
            fn unlink_file(&self, path: &std::path::Path) -> io::Result<()> {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let cohort: u8 = if name.starts_with('0') { 0 } else { 1 };
                self.timeline.lock().unwrap().push((cohort, "start"));
                // Sleep gives rayon room to interleave cohorts if
                // cross-cohort serialisation is broken; with strict
                // ordering it just stretches the wall clock by ~10 ms.
                std::thread::sleep(Duration::from_millis(10));
                self.timeline.lock().unwrap().push((cohort, "finish"));
                Ok(())
            }
            fn rmdir(&self, _path: &std::path::Path) -> io::Result<()> {
                unreachable!()
            }
            fn unlink_symlink(&self, _path: &std::path::Path) -> io::Result<()> {
                unreachable!()
            }
            fn unlink_device(&self, _path: &std::path::Path) -> io::Result<()> {
                unreachable!()
            }
            fn unlink_special(&self, _path: &std::path::Path) -> io::Result<()> {
                unreachable!()
            }
            fn remove_dir_all(&self, _path: &std::path::Path) -> io::Result<()> {
                unreachable!()
            }
            #[cfg(unix)]
            fn unlink_file_at(
                &self,
                _parent_fd: std::os::fd::BorrowedFd<'_>,
                _name: &std::ffi::OsStr,
            ) -> io::Result<()> {
                unreachable!()
            }
            #[cfg(unix)]
            fn rmdir_at(
                &self,
                _parent_fd: std::os::fd::BorrowedFd<'_>,
                _name: &std::ffi::OsStr,
            ) -> io::Result<()> {
                unreachable!()
            }
            #[cfg(unix)]
            fn unlink_symlink_at(
                &self,
                _parent_fd: std::os::fd::BorrowedFd<'_>,
                _name: &std::ffi::OsStr,
            ) -> io::Result<()> {
                unreachable!()
            }
            #[cfg(unix)]
            fn unlink_device_at(
                &self,
                _parent_fd: std::os::fd::BorrowedFd<'_>,
                _name: &std::ffi::OsStr,
            ) -> io::Result<()> {
                unreachable!()
            }
            #[cfg(unix)]
            fn unlink_special_at(
                &self,
                _parent_fd: std::os::fd::BorrowedFd<'_>,
                _name: &std::ffi::OsStr,
            ) -> io::Result<()> {
                unreachable!()
            }
            #[cfg(unix)]
            fn remove_dir_all_at(
                &self,
                _parent_fd: std::os::fd::BorrowedFd<'_>,
                _name: &std::ffi::OsStr,
            ) -> io::Result<()> {
                unreachable!()
            }
        }

        let timeline = Arc::new(Mutex::new(Vec::new()));
        let fs = OrderingFs {
            timeline: Arc::clone(&timeline),
        };
        let emitter = ParallelDeleteEmitter::new(fs);
        emitter
            .enqueue_cohort(
                key("dir0"),
                0,
                vec![cohort_op("0a"), cohort_op("0b"), cohort_op("0c")],
            )
            .unwrap();
        emitter
            .enqueue_cohort(
                key("dir1"),
                1,
                vec![cohort_op("1a"), cohort_op("1b"), cohort_op("1c")],
            )
            .unwrap();
        emitter.mark_producers_done();
        let outcome = run_with_timeout(emitter, Duration::from_secs(5));
        assert_eq!(outcome.stats.files, 6, "all six dispatches completed");

        let timeline = timeline.lock().unwrap().clone();
        let last_cohort0_finish = timeline
            .iter()
            .rposition(|(c, tag)| *c == 0 && *tag == "finish")
            .expect("cohort 0 must produce a finish event");
        let first_cohort1_start = timeline
            .iter()
            .position(|(c, tag)| *c == 1 && *tag == "start")
            .expect("cohort 1 must produce a start event");
        assert!(
            last_cohort0_finish < first_cohort1_start,
            "cross-cohort ordering violated: cohort 1 started before cohort 0 finished. timeline={timeline:?}"
        );
    }

    fn cohort_op(name: &str) -> DeleteOperation {
        DeleteOperation::new(
            PathBuf::from(format!("/dst/{name}")),
            OsString::from(name),
            DeleteEntryKind::File,
        )
    }

    fn run_with_timeout<F: DeleteFs + Sync + Send + 'static>(
        emitter: ParallelDeleteEmitter<F>,
        timeout: Duration,
    ) -> ParallelDrainOutcome<F> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = emitter.run();
            let _ = tx.send(result);
        });
        rx.recv_timeout(timeout)
            .expect(
                "parallel consumer drain timed out - cross-cohort serialisation likely deadlocked",
            )
            .expect("parallel consumer drain returned an error")
    }
}
