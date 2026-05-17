//! [`DeleteContext`] - per-transfer wiring that ties the flist segment
//! consumer to the parallel-deterministic-delete pipeline and drives the
//! [`DeleteEmitter`] for every `--delete-*` timing mode.
//!
//! This module unifies two responsibilities introduced across the DDP
//! task series:
//!
//! 1. The receiver-side observation API ([`Self::observe_segment_for_delete`])
//!    landed in DDP-B3. The receiver calls it once per INC_RECURSE segment;
//!    the context resolves the destination directory, computes per-directory
//!    extras via [`compute_extras`], publishes a sorted [`DeletePlan`] into
//!    the shared [`DeletePlanMap`], and records child directories with the
//!    [`DirTraversalCursor`] so the emitter can yield directories in
//!    upstream `f_name_cmp` ascending order.
//! 2. The timing-mode drain API (DDP-E1-E5). Each `--delete-*` timing mode
//!    keeps the observable semantics it had under the legacy batched-sweep
//!    path, but every unlink, itemize line, and stats counter now flows
//!    through the single-threaded [`DeleteEmitter`] drain.
//!
//! # Wiring per timing mode
//!
//! | Mode             | Phase 1 (plan publish)                | Phase 2 (drain)                                      |
//! |------------------|---------------------------------------|------------------------------------------------------|
//! | `--delete-before`| pre-walk pass over every dir          | [`DeleteContext::emit_all`] before the copy walk     |
//! | `--delete-during`| per-dir inside the copy walk          | [`DeleteContext::emit_one`] before the dir's copies  |
//! | `--delete-after` | per-dir inside the copy walk          | [`DeleteContext::emit_all`] after the copy walk      |
//! | `--delete-delay` | per-dir inside the copy walk          | [`DeleteContext::emit_all`] after all renames commit |
//! | `--delete-excluded` (layered) | upstream of [`compute_extras`] - filter-excluded entries are appended to the segment-extras set | per timing mode above |
//!
//! The legacy batched sweep was retired in DDP-F3 (#2272); the emitter
//! is now the sole production unlink path for every timing mode.
//!
//! # Concurrency
//!
//! [`DeletePlanMap`] already provides interior mutability via a global
//! mutex; the traversal cursor is wrapped in a [`Mutex`] here. The
//! observation API takes `&self`, so the context can live inside an
//! [`Arc`] shared between the receiver and worker threads. The drain
//! consumes the context by value (`mut self`) and is therefore the
//! single-writer path that owns the emitter.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use protocol::flist::FileEntry;

use super::emitter::{DeleteEmitter, DeleteFs, EmitterErrorPolicy};
use super::error::DeleteError;
use super::extras::compute_extras;
use super::plan::DeletePlan;
use super::plan_map::DeletePlanMap;
use super::traversal::DirTraversalCursor;

/// Re-exposes the four upstream timing modes so the emitter and its
/// context can be configured without pulling in the engine's
/// `LocalCopyOptions` type. The variants match
/// [`crate::local_copy::DeleteTiming`] one-for-one; conversion is
/// provided via [`From`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EmitterTiming {
    /// Run the drain before any content transfer.
    Before,
    /// Run the drain interleaved with content transfer, one directory
    /// at a time, before each directory's per-file copies.
    During,
    /// Accumulate plans during transfer; drain after all transfers
    /// complete.
    After,
    /// Accumulate plans during transfer; drain after all renames have
    /// committed.
    Delay,
}

impl EmitterTiming {
    /// Returns `true` for timing modes that drain inside the per-directory
    /// copy loop (only `During`).
    #[must_use]
    pub const fn drains_per_directory(self) -> bool {
        matches!(self, Self::During)
    }

    /// Returns `true` for timing modes that drain after every transfer
    /// (`After` and `Delay`).
    #[must_use]
    pub const fn drains_post_transfer(self) -> bool {
        matches!(self, Self::After | Self::Delay)
    }

    /// Returns `true` for timing modes that drain before any transfer
    /// (only `Before`).
    #[must_use]
    pub const fn drains_pre_transfer(self) -> bool {
        matches!(self, Self::Before)
    }
}

/// Per-transfer context bundling the shared phase-1 state with the
/// emitter configuration that drives the phase-2 drain.
///
/// One [`DeleteContext`] is created per transfer. The receiver wraps the
/// context in [`Arc`] and threads the observation API to its segment
/// hook. The same context is consumed by the drain (`emit_one` /
/// `emit_all`) once phase 1 completes, at which point the [`DeleteFs`]
/// dispatcher is handed in.
#[derive(Debug)]
pub struct DeleteContext {
    /// Shared concurrent map of per-directory plans. Populated by the
    /// receiver hook (DDP-B3) or by inline `compute_extras` calls in the
    /// local-copy executor; drained by the emitter.
    pub plans: Arc<DeletePlanMap>,
    /// Shared cursor that yields directories in upstream traversal order.
    pub cursor: Arc<Mutex<DirTraversalCursor>>,
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
    /// Policy used to instantiate the [`DeleteEmitter`] when the drain
    /// runs.
    pub policy: EmitterErrorPolicy,
    /// Names of entries the segment knows about for the current directory
    /// being planned. Reset each time [`Self::begin_directory`] is
    /// called.
    segment_entries: Mutex<Vec<FileEntry>>,
}

impl DeleteContext {
    /// Builds a new context rooted at `dest_root` with the given timing
    /// mode. The traversal cursor is rooted at the empty relative path
    /// (matching the destination root itself, which upstream
    /// `delete_in_dir` visits first).
    #[must_use]
    pub fn new(dest_root: PathBuf, timing: EmitterTiming) -> Self {
        Self {
            plans: Arc::new(DeletePlanMap::new()),
            cursor: Arc::new(Mutex::new(DirTraversalCursor::new(PathBuf::new()))),
            dest_root,
            enabled: true,
            timing,
            delete_excluded: false,
            policy: EmitterErrorPolicy::default(),
            segment_entries: Mutex::new(Vec::new()),
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
        Self {
            plans,
            cursor: Arc::new(Mutex::new(DirTraversalCursor::new(PathBuf::new()))),
            dest_root,
            enabled,
            timing: EmitterTiming::During,
            delete_excluded: false,
            policy: EmitterErrorPolicy::default(),
            segment_entries: Mutex::new(Vec::new()),
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
        Self {
            plans,
            cursor: Arc::new(Mutex::new(DirTraversalCursor::new(cursor_root))),
            dest_root,
            enabled,
            timing: EmitterTiming::During,
            delete_excluded: false,
            policy: EmitterErrorPolicy::default(),
            segment_entries: Mutex::new(Vec::new()),
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
    /// 5. Records the segment's children in [`Self::cursor`] via
    ///    [`DirTraversalCursor::observe_segment`].
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
    /// Panics if the cursor mutex is poisoned. A poisoned mutex
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

        self.cursor
            .lock()
            .expect("DeleteContext cursor mutex poisoned")
            .observe_segment(dir.to_path_buf(), entries);

        Ok(())
    }

    /// Registers a child directory observation with the cursor so the
    /// emitter sees parents before children. Callers invoke this whenever
    /// a directory's contents become known via an inline directory walk
    /// in the non-INC_RECURSE path.
    pub fn observe_directory(&self, parent: PathBuf, children: &[FileEntry]) {
        self.cursor
            .lock()
            .expect("DeleteContext cursor mutex poisoned")
            .observe_segment(parent, children);
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
    /// # Errors
    ///
    /// Surfaces any fatal error from [`DeleteEmitter::emit_all`].
    pub fn emit_one<F: DeleteFs>(self, fs: F) -> io::Result<DrainOutcome<F>> {
        let mut emitter = self.into_emitter(fs)?;
        emitter.emit_all()?;
        Ok(DrainOutcome::from_emitter(emitter))
    }

    /// Drains every published plan through a freshly-built emitter. Used
    /// by `Before` (pre-walk pass), `After`, and `Delay` (post-transfer
    /// drain).
    ///
    /// # Errors
    ///
    /// Surfaces any fatal error from [`DeleteEmitter::emit_all`].
    pub fn emit_all<F: DeleteFs>(self, fs: F) -> io::Result<DrainOutcome<F>> {
        self.emit_one(fs)
    }

    /// Builds an emitter from this context.
    ///
    /// The plan map and cursor are extracted from their `Arc` wrappers;
    /// callers must release any other clones (typically held by the
    /// receiver) before calling the drain. Returns a typed
    /// [`DeleteError`] (mapped to [`io::Error`] at the public boundary)
    /// when a handle is still shared or the cursor mutex is poisoned.
    /// The error carries the observed [`Arc::strong_count`] so a leaked
    /// clone is visible in operator diagnostics.
    fn into_emitter<F: DeleteFs>(self, fs: F) -> Result<DeleteEmitter<F>, DeleteError> {
        let plans = Arc::try_unwrap(self.plans).map_err(|still_shared| {
            DeleteError::PlanMapStillShared {
                strong_count: Arc::strong_count(&still_shared),
            }
        })?;
        let cursor = Arc::try_unwrap(self.cursor)
            .map_err(|still_shared| DeleteError::CursorStillShared {
                strong_count: Arc::strong_count(&still_shared),
            })?
            .into_inner()
            .map_err(|_| DeleteError::CursorPoisoned)?;
        Ok(DeleteEmitter::with_policy(fs, plans, cursor, self.policy))
    }
}

/// Result of draining one or more directories through the emitter.
///
/// Owns the [`DeleteFs`] so test callers using [`super::RecordingDeleteFs`]
/// can inspect the recorded event sequence after the drain returns.
pub struct DrainOutcome<F: DeleteFs> {
    /// The filesystem dispatcher the emitter consumed. Production code
    /// drops this; tests inspect `events()` on a `RecordingDeleteFs`.
    pub fs: F,
    /// Running deletion statistics, mutated only inside the drain.
    pub stats: protocol::DeleteStats,
    /// Accumulated `io_error` bitmask the caller maps to an exit code.
    pub io_error: i32,
    /// Mapped exit code (`0`, `23`, or `24`) for the run.
    pub exit_code: i32,
}

impl<F: DeleteFs> DrainOutcome<F> {
    fn from_emitter(emitter: DeleteEmitter<F>) -> Self {
        let stats = emitter.stats();
        let io_error = emitter.io_error();
        let exit_code = emitter.exit_code();
        let fs = emitter.into_fs();
        Self {
            fs,
            stats,
            io_error,
            exit_code,
        }
    }
}

impl From<crate::local_copy::DeleteTiming> for EmitterTiming {
    fn from(value: crate::local_copy::DeleteTiming) -> Self {
        match value {
            crate::local_copy::DeleteTiming::Before => Self::Before,
            crate::local_copy::DeleteTiming::During => Self::During,
            crate::local_copy::DeleteTiming::After => Self::After,
            crate::local_copy::DeleteTiming::Delay => Self::Delay,
        }
    }
}

impl From<EmitterTiming> for crate::local_copy::DeleteTiming {
    fn from(value: EmitterTiming) -> Self {
        match value {
            EmitterTiming::Before => Self::Before,
            EmitterTiming::During => Self::During,
            EmitterTiming::After => Self::After,
            EmitterTiming::Delay => Self::Delay,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::fs::File;

    use tempfile::TempDir;

    use super::super::emitter::{DeleteEvent, RecordingDeleteFs};
    use super::super::plan::DeleteEntryKind;
    use super::*;

    fn touch(dir: &Path, name: &str) {
        File::create(dir.join(name)).expect("create file");
    }

    fn flist_file(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    fn flist_dir(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    fn make_segment(names: &[&str]) -> Vec<FileEntry> {
        names.iter().map(|n| flist_file(n)).collect()
    }

    fn dir_child(parent: &str, name: &str) -> FileEntry {
        let path = if parent.is_empty() {
            PathBuf::from(name)
        } else {
            PathBuf::from(parent).join(name)
        };
        FileEntry::new_directory(path, 0o755)
    }

    #[test]
    fn disabled_context_publishes_nothing() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "extra");
        let map = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&map), dir.path().to_path_buf(), false);

        ctx.observe_segment_for_delete(Path::new(""), &[flist_file("kept")])
            .expect("disabled is a no-op");

        assert!(map.is_empty());
        let mut cursor = ctx.cursor.lock().unwrap();
        // Even with no observations, the root is still emitted, and the
        // second call drains the now-empty stack and reports exhaustion.
        assert_eq!(cursor.next_ready(), Some(PathBuf::new()));
        assert_eq!(cursor.next_ready(), None);
        assert!(cursor.is_exhausted());
    }

    #[test]
    fn enabled_context_publishes_sorted_plan_and_records_children() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        for n in ["a", "b", "c", "d"] {
            touch(&sub, n);
        }

        let map = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&map), dir.path().to_path_buf(), true);

        let root_segment = vec![flist_dir("sub")];
        ctx.observe_segment_for_delete(Path::new(""), &root_segment)
            .expect("root observe ok");

        let segment = vec![flist_file("a"), flist_file("c"), flist_dir("nested")];
        ctx.observe_segment_for_delete(Path::new("sub"), &segment)
            .expect("observe ok");

        assert!(map.contains(Path::new("sub")));
        let plan = map.take(Path::new("sub")).expect("plan present");
        assert!(plan.is_sorted());
        let names: Vec<&OsStr> = plan.extras.iter().map(|e| e.name.as_os_str()).collect();
        assert_eq!(names, vec![OsStr::new("d"), OsStr::new("b")]);

        let mut cursor = ctx.cursor.lock().unwrap();
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert!(seq.contains(&PathBuf::from("sub/nested")));
    }

    #[test]
    fn accumulates_plans_across_segments() {
        let root = TempDir::new().unwrap();
        for sub in ["s1", "s2", "s3"] {
            let p = root.path().join(sub);
            fs::create_dir(&p).unwrap();
            touch(&p, "keeper");
            touch(&p, "trash");
        }

        let map = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&map), root.path().to_path_buf(), true);

        for sub in ["s1", "s2", "s3"] {
            let entries = vec![flist_file("keeper")];
            ctx.observe_segment_for_delete(Path::new(sub), &entries)
                .expect("observe ok");
        }

        assert_eq!(map.len(), 3);
        for sub in ["s1", "s2", "s3"] {
            let plan = map.take(Path::new(sub)).expect("plan present");
            assert_eq!(plan.extras.len(), 1);
            assert_eq!(plan.extras[0].name, std::ffi::OsString::from("trash"));
        }
    }

    #[test]
    fn missing_destination_dir_surfaces_io_error() {
        let root = TempDir::new().unwrap();
        let map = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&map), root.path().to_path_buf(), true);

        let err = ctx
            .observe_segment_for_delete(Path::new("does-not-exist"), &[])
            .expect_err("missing dir is an error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(map.is_empty());
    }

    #[test]
    fn with_cursor_root_uses_provided_root() {
        let root = TempDir::new().unwrap();
        let map = Arc::new(DeletePlanMap::new());
        let ctx = DeleteContext::with_cursor_root(
            Arc::clone(&map),
            root.path().to_path_buf(),
            PathBuf::from("from_here"),
            true,
        );

        let mut cursor = ctx.cursor.lock().unwrap();
        assert_eq!(cursor.next_ready(), Some(PathBuf::from("from_here")));
    }

    #[test]
    fn empty_segment_still_publishes_plan_for_dest_only_entries() {
        let root = TempDir::new().unwrap();
        touch(root.path(), "ghost1");
        touch(root.path(), "ghost2");

        let map = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&map), root.path().to_path_buf(), true);
        ctx.observe_segment_for_delete(Path::new(""), &[])
            .expect("observe ok");

        let plan = map.take(Path::new("")).expect("plan present");
        assert_eq!(plan.extras.len(), 2);
        assert!(plan.is_sorted());
    }

    #[test]
    fn timing_predicates_partition_modes() {
        assert!(EmitterTiming::Before.drains_pre_transfer());
        assert!(EmitterTiming::During.drains_per_directory());
        assert!(EmitterTiming::After.drains_post_transfer());
        assert!(EmitterTiming::Delay.drains_post_transfer());
        assert!(!EmitterTiming::Before.drains_per_directory());
        assert!(!EmitterTiming::During.drains_post_transfer());
    }

    #[test]
    fn round_trip_with_local_copy_delete_timing() {
        for mode in [
            EmitterTiming::Before,
            EmitterTiming::During,
            EmitterTiming::After,
            EmitterTiming::Delay,
        ] {
            let lc: crate::local_copy::DeleteTiming = mode.into();
            let back: EmitterTiming = lc.into();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn during_mode_drains_one_directory_through_emitter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("sub");
        fs::create_dir(&dir).unwrap();
        for n in ["keep", "drop"] {
            touch(&dir, n);
        }

        let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
        ctx.observe_directory(dir.clone(), &[]);
        ctx.begin_directory(make_segment(&["keep"]));
        ctx.publish_plan_for(&dir).expect("publish plan");

        let outcome = ctx
            .emit_one(RecordingDeleteFs::new())
            .expect("drain succeeds");
        let events = outcome.fs.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, dir.join("drop"));
        assert_eq!(events[0].kind, DeleteEntryKind::File);
        assert_eq!(outcome.stats.files, 1);
        assert_eq!(outcome.exit_code, 0);
    }

    #[test]
    fn before_mode_drains_pre_walk_pass_across_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        touch(&a, "x");
        touch(&b, "y");

        let ctx = DeleteContext::new(tmp.path().to_path_buf(), EmitterTiming::Before);
        ctx.observe_directory(
            tmp.path().to_path_buf(),
            &[
                dir_child(tmp.path().to_str().unwrap(), "a"),
                dir_child(tmp.path().to_str().unwrap(), "b"),
            ],
        );
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(tmp.path()).unwrap();
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&a).unwrap();
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&b).unwrap();

        let outcome = ctx
            .emit_all(RecordingDeleteFs::new())
            .expect("drain succeeds");
        let names: Vec<PathBuf> = outcome.fs.events().iter().map(|e| e.path.clone()).collect();
        assert!(names.iter().any(|p| p == &a.join("x")));
        assert!(names.iter().any(|p| p == &b.join("y")));
        assert_eq!(outcome.stats.files, 2);
    }

    #[test]
    fn after_mode_accumulates_then_drains() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        touch(&dir, "later1");
        touch(&dir, "later2");

        let ctx = DeleteContext::new(dir.clone(), EmitterTiming::After);
        ctx.observe_directory(dir.clone(), &[]);
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&dir).unwrap();

        let outcome = ctx
            .emit_all(RecordingDeleteFs::new())
            .expect("drain succeeds");
        assert_eq!(outcome.stats.files, 2);
        assert_eq!(outcome.exit_code, 0);
    }

    #[test]
    fn delay_mode_uses_same_drain_path_as_after() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        touch(&dir, "delayed");

        let ctx = DeleteContext::new(dir.clone(), EmitterTiming::Delay);
        ctx.observe_directory(dir.clone(), &[]);
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&dir).unwrap();

        let outcome = ctx
            .emit_all(RecordingDeleteFs::new())
            .expect("drain succeeds");
        assert_eq!(outcome.stats.files, 1);
    }

    #[test]
    fn delete_excluded_layering_bit_round_trips() {
        let ctx = DeleteContext::new(PathBuf::from("/"), EmitterTiming::During)
            .with_delete_excluded(true);
        assert!(ctx.delete_excluded);
        let ctx = DeleteContext::new(PathBuf::from("/"), EmitterTiming::During);
        assert!(!ctx.delete_excluded);
    }

    #[test]
    fn into_emitter_reports_plan_map_still_shared_with_strong_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plans = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&plans), tmp.path().to_path_buf(), true);
        // The caller still holds `plans`; the drain must surface the
        // residual strong-count via the typed error.
        let err = ctx
            .into_emitter(RecordingDeleteFs::new())
            .expect_err("plan map is still shared");
        match err {
            DeleteError::PlanMapStillShared { strong_count } => {
                assert!(
                    strong_count >= 2,
                    "expected strong_count >= 2, got {strong_count}"
                );
            }
            other => panic!("expected PlanMapStillShared, got {other:?}"),
        }
        drop(plans);
    }

    #[test]
    fn into_emitter_reports_cursor_still_shared_with_strong_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = DeleteContext::new(tmp.path().to_path_buf(), EmitterTiming::During);
        // Leak a clone of the cursor so the drain cannot unwrap it.
        let _leaked_cursor = Arc::clone(&ctx.cursor);
        let err = ctx
            .into_emitter(RecordingDeleteFs::new())
            .expect_err("cursor is still shared");
        match err {
            DeleteError::CursorStillShared { strong_count } => {
                assert!(
                    strong_count >= 2,
                    "expected strong_count >= 2, got {strong_count}"
                );
            }
            other => panic!("expected CursorStillShared, got {other:?}"),
        }
    }

    #[test]
    fn emit_one_propagates_typed_error_through_io_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plans = Arc::new(DeletePlanMap::new());
        let ctx =
            DeleteContext::with_shared_plan_map(Arc::clone(&plans), tmp.path().to_path_buf(), true);
        let err = ctx
            .emit_one(RecordingDeleteFs::new())
            .expect_err("plan map still shared surfaces as io::Error");
        let msg = err.to_string();
        assert!(msg.contains("DeletePlanMap"));
        assert!(msg.contains("strong_count="));
        drop(plans);
    }

    #[test]
    fn record_drain_outcome_carries_recorded_events() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        touch(&dir, "victim");
        let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
        ctx.observe_directory(dir.clone(), &[]);
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&dir).unwrap();
        let outcome = ctx.emit_one(RecordingDeleteFs::new()).unwrap();
        assert_eq!(
            outcome.fs.events(),
            vec![DeleteEvent {
                path: dir.join("victim"),
                kind: DeleteEntryKind::File,
            }]
        );
    }
}
