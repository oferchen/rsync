//! [`DeleteContext`] - per-transfer wiring that ties the flist segment
//! consumer to the parallel-deterministic-delete pipeline and drives the
//! `DeleteEmitter` for every `--delete-*` timing mode.
//!
//! See the parent module documentation for the wiring tables and
//! channel-based cursor handoff design.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, unbounded};
use protocol::flist::FileEntry;

#[cfg(not(feature = "parallel-delete-consumer"))]
use super::super::emitter::DeleteEmitter;
use super::super::emitter::{DeleteFs, EmitterErrorPolicy};
use super::super::error::DeleteError;
use super::super::extras::compute_extras;
use super::super::plan::DeletePlan;
use super::super::plan_map::DeletePlanMap;
use super::super::traversal::DirTraversalCursor;
use super::outcome::DrainOutcome;
use super::timing::EmitterTiming;

/// One cursor observation enqueued by a worker thread for the emitter to
/// fold into its [`DirTraversalCursor`] at drain time.
///
/// Carrying owned `PathBuf` and `Vec<FileEntry>` lets the producer return
/// immediately without holding any lock on the cursor itself; the consumer
/// builds the cursor lazily in [`DeleteContext::into_emitter`].
#[derive(Debug)]
pub(super) struct CursorObservation {
    pub(super) dir: PathBuf,
    pub(super) children: Vec<FileEntry>,
}

/// Per-transfer context bundling the shared phase-1 state with the
/// emitter configuration that drives the phase-2 drain.
///
/// One [`DeleteContext`] is created per transfer. The receiver wraps the
/// context in [`Arc`] and threads the observation API to its segment
/// hook. The same context is consumed by the drain (`emit_one` /
/// `emit_all`) once phase 1 completes, at which point the [`DeleteFs`]
/// dispatcher is handed in.
///
/// # Channel-based cursor handoff
///
/// Cursor observations from workers are queued through an
/// [`crossbeam_channel::unbounded`] producer-consumer channel rather than
/// shared via `Arc<Mutex<DirTraversalCursor>>`. Workers send owned
/// observations; the drain (`into_emitter`) drops its sender, drains the
/// receiver until it returns `None`, and builds the owned cursor in one
/// place. This eliminates the `Arc::try_unwrap` on the cursor handle and
/// makes shutdown deterministic when workers drop normally or panic mid-
/// send.
#[derive(Debug)]
pub struct DeleteContext {
    /// Shared concurrent map of per-directory plans. Populated by the
    /// receiver hook (DDP-B3) or by inline `compute_extras` calls in the
    /// local-copy executor; drained by the emitter.
    pub plans: Arc<DeletePlanMap>,
    /// Destination root the receiver writes into. Per-segment relative
    /// directory paths are resolved relative to this when computing
    /// extras inside [`Self::observe_segment_for_delete`].
    pub dest_root: PathBuf,
    /// Master switch. When `false`, [`Self::observe_segment_for_delete`]
    /// is a no-op so callers can wire the context unconditionally and
    /// let it stay dormant when the transfer is not in a delete mode.
    pub enabled: bool,
    /// Selected `--delete-*` timing mode.
    pub timing: EmitterTiming,
    /// Whether `--delete-excluded` is layered on top of the timing mode.
    /// When `true`, filter-excluded names are appended to the
    /// segment-extras set before [`compute_extras`] runs (see section 5
    /// of the design).
    pub delete_excluded: bool,
    /// Policy used to instantiate the `DeleteEmitter` when the drain
    /// runs.
    pub policy: EmitterErrorPolicy,
    /// Names of entries the segment knows about for the current directory
    /// being planned. Reset each time [`Self::begin_directory`] is
    /// called.
    pub(super) segment_entries: Mutex<Vec<FileEntry>>,
    /// Root path used to seed the [`DirTraversalCursor`] at drain time.
    pub(super) cursor_root: PathBuf,
    /// Producer side of the cursor observation channel. Workers calling
    /// [`Self::observe_segment_for_delete`] enqueue observations here.
    /// Wrapped in `Mutex<Option<_>>` so [`Self::into_emitter`] can drop
    /// the master sender to close the channel from the drain side.
    pub(super) cursor_tx: Mutex<Option<Sender<CursorObservation>>>,
    /// Consumer side of the cursor observation channel. Owned by the
    /// drain; taken on first call into [`Self::into_emitter`] and then
    /// drained until the channel reports closure.
    pub(super) cursor_rx: Mutex<Option<Receiver<CursorObservation>>>,
}

impl DeleteContext {
    /// Builds a new context rooted at `dest_root` with the given timing
    /// mode. The traversal cursor is seated at `dest_root` so the first
    /// directory the emitter pulls matches the key under which callers
    /// publish their plan via [`Self::publish_plan_for`] or by inserting
    /// directly into [`Self::plans`]. Use [`Self::with_shared_plan_map`]
    /// for the receiver-driven pipeline where plans are keyed by
    /// destination-relative paths and the cursor must start at the
    /// relative root.
    #[must_use]
    pub fn new(dest_root: PathBuf, timing: EmitterTiming) -> Self {
        let (tx, rx) = unbounded();
        let cursor_root = dest_root.clone();
        Self {
            plans: Arc::new(DeletePlanMap::new()),
            dest_root,
            enabled: true,
            timing,
            delete_excluded: false,
            policy: EmitterErrorPolicy::default(),
            segment_entries: Mutex::new(Vec::new()),
            cursor_root,
            cursor_tx: Mutex::new(Some(tx)),
            cursor_rx: Mutex::new(Some(rx)),
        }
    }

    /// Builds a context that shares an existing [`DeletePlanMap`] with
    /// the receiver-side phase-1 workers. Use this when the caller has
    /// already constructed the plan map (for example, when the receiver
    /// wants to keep its own handle for inspection).
    #[must_use]
    pub fn with_shared_plan_map(
        plans: Arc<DeletePlanMap>,
        dest_root: PathBuf,
        enabled: bool,
    ) -> Self {
        let (tx, rx) = unbounded();
        Self {
            plans,
            dest_root,
            enabled,
            timing: EmitterTiming::During,
            delete_excluded: false,
            policy: EmitterErrorPolicy::default(),
            segment_entries: Mutex::new(Vec::new()),
            cursor_root: PathBuf::new(),
            cursor_tx: Mutex::new(Some(tx)),
            cursor_rx: Mutex::new(Some(rx)),
        }
    }

    /// Builds a context whose traversal cursor is rooted at `cursor_root`
    /// rather than the empty path. Useful when the caller wants the
    /// emitter to begin its drain at a specific subtree (for example,
    /// when the transfer's source is a single directory below the
    /// destination root).
    #[must_use]
    pub fn with_cursor_root(
        plans: Arc<DeletePlanMap>,
        dest_root: PathBuf,
        cursor_root: PathBuf,
        enabled: bool,
    ) -> Self {
        let (tx, rx) = unbounded();
        Self {
            plans,
            dest_root,
            enabled,
            timing: EmitterTiming::During,
            delete_excluded: false,
            policy: EmitterErrorPolicy::default(),
            segment_entries: Mutex::new(Vec::new()),
            cursor_root,
            cursor_tx: Mutex::new(Some(tx)),
            cursor_rx: Mutex::new(Some(rx)),
        }
    }

    /// Sets the `--delete-excluded` layering bit.
    #[must_use]
    pub fn with_delete_excluded(mut self, enabled: bool) -> Self {
        self.delete_excluded = enabled;
        self
    }

    /// Overrides the emitter error policy.
    #[must_use]
    pub fn with_policy(mut self, policy: EmitterErrorPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Observes one flist segment from the receiver and publishes the
    /// corresponding [`DeletePlan`] into the plan map.
    ///
    /// `dir` is the segment's destination-relative content directory.
    /// `entries` is the segment's full child list (files + dirs +
    /// symlinks + everything else). The same slice is forwarded to the
    /// traversal cursor so it can record child directories for the
    /// emitter's depth-first walk.
    ///
    /// # Behaviour
    ///
    /// 1. When `self.enabled` is `false`, returns `Ok(())` without any
    ///    side effect.
    /// 2. Resolves `self.dest_root.join(dir)` and calls
    ///    [`compute_extras`] to obtain the unsorted candidate list.
    /// 3. Wraps the candidates in a [`DeletePlan`] and calls
    ///    [`DeletePlan::sort_by_name`] to lock in upstream emission
    ///    order.
    /// 4. Inserts the plan into [`Self::plans`] keyed by `dir`.
    /// 5. Enqueues a `CursorObservation` onto the producer channel.
    ///    The drain in `Self::into_emitter` consumes the channel after
    ///    all senders drop and folds each observation into the owned
    ///    [`DirTraversalCursor`].
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from [`compute_extras`] when the
    /// destination directory cannot be read. The receiver caller is
    /// expected to log and continue rather than abort the transfer, the
    /// existing batched-sweep path will still run, matching upstream's
    /// `io_error |= 1` behaviour for `read_dir` failures.
    ///
    /// # Panics
    ///
    /// Panics if the cursor sender mutex is poisoned. A poisoned mutex
    /// indicates an unrecoverable bug in the emitter side and is
    /// treated the same way the plan map treats poisoned state.
    pub fn observe_segment_for_delete(&self, dir: &Path, entries: &[FileEntry]) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let dest_dir = self.dest_root.join(dir);
        let extras = compute_extras(&dest_dir, entries)?;
        let mut plan = DeletePlan::from_extras(dir.to_path_buf(), extras);
        plan.sort_by_name();
        self.plans.insert(plan);

        self.send_cursor_observation(dir.to_path_buf(), entries.to_vec());

        Ok(())
    }

    /// Registers a child directory observation with the cursor so the
    /// emitter sees parents before children. Callers invoke this whenever
    /// a directory's contents become known via an inline directory walk
    /// in the non-INC_RECURSE path.
    pub fn observe_directory(&self, parent: PathBuf, children: &[FileEntry]) {
        self.send_cursor_observation(parent, children.to_vec());
    }

    /// Enqueues a cursor observation onto the producer channel.
    ///
    /// A closed channel (sender already taken by [`Self::into_emitter`])
    /// drops the observation silently. This matches the upstream
    /// `delete_in_dir` contract: late observations after the drain has
    /// committed to its traversal order are ignored, exactly like
    /// [`DirTraversalCursor::observe_segment`] when invoked past the
    /// parent's frame.
    fn send_cursor_observation(&self, dir: PathBuf, children: Vec<FileEntry>) {
        let guard = self
            .cursor_tx
            .lock()
            .expect("DeleteContext cursor_tx mutex poisoned");
        if let Some(tx) = guard.as_ref() {
            // crossbeam unbounded send only fails when the receiver is
            // dropped, which happens after the drain has consumed
            // everything. Treat that as a no-op for the same reason
            // late observations are tolerated above.
            let _ = tx.send(CursorObservation { dir, children });
        }
    }

    /// Returns a freshly built [`DirTraversalCursor`] reflecting every
    /// observation queued so far.
    ///
    /// Pending channel messages are drained, applied to the new cursor,
    /// and then re-enqueued so the eventual `Self::into_emitter` drain
    /// still sees the complete observation set. The producer channel
    /// remains open; future `observe_*` calls continue to land on it.
    ///
    /// Intended for tests and diagnostics that want to inspect the
    /// emission order without consuming the context. Re-enqueueing keeps
    /// inspection observable while preserving the channel-shutdown
    /// contract for the drain.
    #[must_use]
    pub fn cursor_snapshot(&self) -> Mutex<DirTraversalCursor> {
        let mut cursor = DirTraversalCursor::new(self.cursor_root.clone());
        let tx_guard = self
            .cursor_tx
            .lock()
            .expect("DeleteContext cursor_tx mutex poisoned");
        let rx_guard = self
            .cursor_rx
            .lock()
            .expect("DeleteContext cursor_rx mutex poisoned");
        let (Some(tx), Some(rx)) = (tx_guard.as_ref(), rx_guard.as_ref()) else {
            return Mutex::new(cursor);
        };

        // Drain everything currently queued so we can replay the
        // observations into a private cursor. We re-clone each message
        // back into the channel so the eventual drain still sees them;
        // the channel is FIFO so ordering is preserved.
        let mut drained: Vec<CursorObservation> = Vec::new();
        while let Ok(obs) = rx.try_recv() {
            drained.push(obs);
        }
        for obs in &drained {
            cursor.observe_segment(obs.dir.clone(), &obs.children);
        }
        for obs in drained {
            // Re-send so the real drain still sees the observation.
            // Failure here means the receiver dropped, which cannot
            // happen while we hold the rx guard.
            let _ = tx.send(obs);
        }

        Mutex::new(cursor)
    }

    /// Records the set of entries the segment for `dir` knows about so a
    /// subsequent [`Self::publish_plan_for`] call can compute extras
    /// inline without re-reading the segment.
    ///
    /// Used by the per-dir wiring in the recursive executor: the planner
    /// has already iterated the source directory; we feed those names
    /// here instead of asking the receiver for them.
    pub fn begin_directory(&self, segment_entries: Vec<FileEntry>) {
        *self
            .segment_entries
            .lock()
            .expect("DeleteContext segment_entries mutex poisoned") = segment_entries;
    }

    /// Computes extras for `dir` against the last [`Self::begin_directory`]
    /// segment, sorts the resulting plan in upstream emission order, and
    /// publishes it into the plan map.
    ///
    /// # Errors
    ///
    /// Surfaces any [`io::Error`] from [`compute_extras`] (typically
    /// `NotFound` when `dir` does not exist at the destination; callers
    /// log and skip in that case).
    pub fn publish_plan_for(&self, dir: &Path) -> io::Result<()> {
        let entries = self
            .segment_entries
            .lock()
            .expect("DeleteContext segment_entries mutex poisoned")
            .clone();
        let extras = compute_extras(dir, &entries)?;
        let mut plan = DeletePlan::from_extras(dir.to_path_buf(), extras);
        plan.sort_by_name();
        self.plans.insert(plan);
        Ok(())
    }

    /// Drains one directory's plan through a freshly-built emitter. Used
    /// by the `During` timing mode at the top of each per-directory copy
    /// step.
    ///
    /// Returns the events produced by the emitter (via the supplied
    /// [`DeleteFs`]) plus the running stats and io_error state surfaced
    /// by the drain.
    ///
    /// # Dispatch
    ///
    /// When the crate is built with `--features parallel-delete-consumer`,
    /// DEL-2.d routes the drain through `ParallelDeleteEmitter` so cohort
    /// dispatch runs on rayon while preserving the DEL-1.a cross-cohort
    /// wire-ordering invariant. The sequential
    /// `DeleteEmitter` remains the default.
    ///
    /// # Errors
    ///
    /// Surfaces any fatal error from [`DeleteEmitter::emit_all`] (or the
    /// parallel consumer's equivalent under the feature flag).
    // DEL-2.d: feature-gated dispatch, parallel-delete-consumer opt-in
    #[cfg(not(feature = "parallel-delete-consumer"))]
    pub fn emit_one<F: DeleteFs>(self, fs: F) -> io::Result<DrainOutcome<F>> {
        let mut emitter = self.into_emitter(fs)?;
        emitter.emit_all()?;
        Ok(DrainOutcome::from_emitter(emitter))
    }

    /// Parallel-feature variant of [`Self::emit_one`]. The `Sync + Send +
    /// 'static` bounds are required by
    /// [`super::super::parallel_consumer::ParallelDeleteEmitter::run`],
    /// which spawns a dedicated consumer thread and shares the dispatcher
    /// across the rayon pool via [`Arc`].
    // DEL-2.d: feature-gated dispatch, parallel-delete-consumer opt-in
    #[cfg(feature = "parallel-delete-consumer")]
    pub fn emit_one<F: DeleteFs + Sync + Send + 'static>(
        self,
        fs: F,
    ) -> io::Result<DrainOutcome<F>> {
        self.emit_via_parallel_consumer(fs)
    }

    /// Drains every published plan through a freshly-built emitter. Used
    /// by `Before` (pre-walk pass), `After`, and `Delay` (post-transfer
    /// drain).
    ///
    /// # Errors
    ///
    /// Surfaces any fatal error from [`DeleteEmitter::emit_all`].
    // DEL-2.d: feature-gated dispatch, parallel-delete-consumer opt-in
    #[cfg(not(feature = "parallel-delete-consumer"))]
    pub fn emit_all<F: DeleteFs>(self, fs: F) -> io::Result<DrainOutcome<F>> {
        self.emit_one(fs)
    }

    /// Parallel-feature variant of [`Self::emit_all`]; bounds widened to
    /// match [`Self::emit_one`].
    // DEL-2.d: feature-gated dispatch, parallel-delete-consumer opt-in
    #[cfg(feature = "parallel-delete-consumer")]
    pub fn emit_all<F: DeleteFs + Sync + Send + 'static>(
        self,
        fs: F,
    ) -> io::Result<DrainOutcome<F>> {
        self.emit_one(fs)
    }

    /// DEL-2.d implementation: translates the published [`DeletePlan`]s
    /// into cohort-keyed [`super::super::reorder_buffer::DeleteOperation`]
    /// batches, walks the cursor to assign monotonic ranks, and runs the
    /// parallel consumer to completion. Returns a [`DrainOutcome`] with
    /// the same shape the sequential emitter produces so callers stay
    /// unchanged across the feature toggle.
    #[cfg(feature = "parallel-delete-consumer")]
    fn emit_via_parallel_consumer<F: DeleteFs + Sync + Send + 'static>(
        self,
        fs: F,
    ) -> io::Result<DrainOutcome<F>> {
        use super::super::parallel_consumer::ParallelDeleteEmitter;
        use super::super::reorder_buffer::{DeleteCohortKey, DeleteOperation};

        let (plans, mut cursor, policy) = self.into_drain_parts().map_err(io::Error::from)?;
        let emitter = ParallelDeleteEmitter::with_policy(fs, policy);
        let mut rank: u64 = 0;
        while let Some(dir) = cursor.next_ready() {
            let Some(plan) = plans.take(&dir) else {
                continue;
            };
            let directory = plan.directory;
            let ops: Vec<DeleteOperation> = plan
                .extras
                .into_iter()
                .map(|entry| {
                    let path = directory.join(&entry.name);
                    DeleteOperation::new(path, entry.name, entry.kind)
                })
                .collect();
            emitter
                .enqueue_cohort(DeleteCohortKey::new(dir), rank, ops)
                .map_err(|err| io::Error::other(err.to_string()))?;
            rank = rank.saturating_add(1);
        }
        emitter.mark_producers_done();
        let outcome = emitter.run()?;
        Ok(DrainOutcome {
            fs: outcome.fs,
            stats: outcome.stats,
            io_error: outcome.io_error,
            exit_code: outcome.exit_code,
        })
    }

    /// Builds an emitter from this context.
    ///
    /// The plan map is extracted from its `Arc` wrapper; callers must
    /// release any other clones (typically held by the receiver) before
    /// calling the drain. The cursor is rebuilt from observations
    /// produced by [`Self::observe_segment_for_delete`] and
    /// [`Self::observe_directory`] using channel-shutdown semantics:
    ///
    /// 1. The master `cursor_tx` sender is dropped, closing the producer
    ///    side from the drain's perspective.
    /// 2. `cursor_rx.recv()` is called in a loop. Because every clone of
    ///    the [`DeleteContext`] holds its sender through the same mutex,
    ///    by the time we own `self` by value no other clone can exist,
    ///    and the loop terminates as soon as the queue empties.
    /// 3. Each observation is applied to a freshly built cursor.
    ///
    /// # Errors
    ///
    /// Returns a typed [`DeleteError`] (mapped to [`io::Error`] at the
    /// public `emit_*` boundary) when the plan map `Arc` is still shared
    /// or the cursor receiver has already been taken. The error carries
    /// the observed [`Arc::strong_count`] so a leaked clone is visible
    /// in operator diagnostics. The cursor side cannot fail with a
    /// "still shared" error under channel-shutdown semantics - workers
    /// only hold sender clones, never `Arc<DirTraversalCursor>`.
    #[cfg(not(feature = "parallel-delete-consumer"))]
    pub(super) fn into_emitter<F: DeleteFs>(self, fs: F) -> Result<DeleteEmitter<F>, DeleteError> {
        let (plans, cursor, policy) = self.into_drain_parts()?;
        Ok(DeleteEmitter::with_policy(fs, plans, cursor, policy))
    }

    /// Extracts the owned drain inputs (plan map, traversal cursor,
    /// emitter policy) from this context. Shared by [`Self::into_emitter`]
    /// (sequential path) and the parallel `emit_via_parallel_consumer`
    /// path under the `parallel-delete-consumer` feature. The
    /// channel-shutdown semantics and `Arc::try_unwrap` invariant are
    /// preserved exactly because both paths consume `self` by value.
    fn into_drain_parts(
        self,
    ) -> Result<(DeletePlanMap, DirTraversalCursor, EmitterErrorPolicy), DeleteError> {
        let plans = Arc::try_unwrap(self.plans).map_err(|still_shared| {
            DeleteError::PlanMapStillShared {
                strong_count: Arc::strong_count(&still_shared),
            }
        })?;

        // Drop the master sender so the receive loop can observe channel
        // closure. Any clones held by workers go away with their
        // `Arc<DeleteContext>` clones; consuming `self` by value here
        // guarantees no clone is alive at this point, so the queue we
        // drain is the final, complete observation set.
        let _ = self
            .cursor_tx
            .lock()
            .expect("DeleteContext cursor_tx mutex poisoned")
            .take();
        let rx = self
            .cursor_rx
            .lock()
            .expect("DeleteContext cursor_rx mutex poisoned")
            .take()
            .ok_or(DeleteError::CursorReceiverAlreadyTaken)?;

        let mut cursor = DirTraversalCursor::new(self.cursor_root);
        while let Ok(obs) = rx.recv() {
            cursor.observe_segment(obs.dir, &obs.children);
        }

        Ok((plans, cursor, self.policy))
    }
}
