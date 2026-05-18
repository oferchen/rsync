//! Single-threaded emitter for the parallel-deterministic delete pipeline.
//!
//! Hosts [`DeleteEmitter`], the drain task that consumes [`DeletePlan`]s
//! from a [`DeletePlanMap`] in the order dictated by [`DirTraversalCursor`]
//! (upstream depth-first traversal) and issues one filesystem operation
//! per planned entry through a [`DeleteFs`] trait.
//!
//! # Task scope
//!
//! - DDP-C1 (#2259) - trait, fakes, scaffold drain loop.
//! - DDP-C2 (#2260) - full dispatch by entry kind matching upstream
//!   `delete.c::delete_item` (`rmdir` for empty dirs, recursive descent on
//!   `ENOTEMPTY` via a nested plan or `remove_dir_all` fallback that
//!   mirrors `delete_dir_contents`, `unlink` for everything else).
//! - DDP-C3 (#2261) - [`EmitterErrorPolicy`] mirroring upstream's
//!   continue-vs-abort behaviour: non-fatal errors set the `io_error`
//!   flag (upstream `IOERR_GENERAL`) and the drain keeps going; fatal
//!   classifications abort and surface an [`io::Error`] mapped to
//!   `RERR_PARTIAL` (23) / `RERR_VANISHED` (24).
//! - DDP-C4 (#2262) - unit tests for synthetic plan sequences.
//!
//! # Submodules
//!
//! - [`fs`] - [`DeleteFs`] trait plus the production [`RealDeleteFs`]
//!   and the [`RecordingDeleteFs`] test fake.
//! - [`policy`] - [`EmitterErrorPolicy`] and the exit-code constants
//!   [`EMITTER_PARTIAL_EXIT_CODE`] / [`EMITTER_VANISHED_EXIT_CODE`].
//! - [`cohort`] - [`CohortDeleteRecord`] surfaced to callers that wire
//!   a hardlink cohort snapshot.
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:48-122`
//!   (`delete_dir_contents`): recursive directory peel used when an
//!   `rmdir` would fail with `ENOTEMPTY`.
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
//!   (`delete_item`): dispatch by `S_ISDIR` / `S_ISLNK` / `IS_DEVICE` /
//!   `IS_SPECIAL`, with `do_rmdir` for directories and `robust_unlink`
//!   for everything else; `ENOTEMPTY` recurses, other errors are logged
//!   and reported via `DR_FAILURE`.
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
//!   (`delete_in_dir`): reverse iteration over the sorted destination
//!   listing, one `delete_item` call per non-matched entry.
//! - `target/interop/upstream-src/rsync-3.4.1/errcode.h`: `RERR_PARTIAL`
//!   (23) and `RERR_VANISHED` (24).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::DeleteStats;

use super::cohort_index::CohortIndex;
use super::plan::HardlinkCohortId;
use super::{DeleteEntryKind, DeletePlan, DeletePlanMap, DirTraversalCursor};

mod cohort;
mod fs;
mod policy;

#[cfg(test)]
mod tests;

pub use cohort::CohortDeleteRecord;
pub use fs::{DeleteEvent, DeleteFs, RealDeleteFs, RecordingDeleteFs};
pub use policy::{EMITTER_PARTIAL_EXIT_CODE, EMITTER_VANISHED_EXIT_CODE, EmitterErrorPolicy};

use policy::{IOERR_GENERAL, IOERR_VANISHED_ONLY};

/// Single-threaded drain task that issues deletions for one transfer.
///
/// Owns a [`DeleteFs`] dispatcher, a counter [`DeleteStats`], the
/// published [`DeletePlanMap`], a [`DirTraversalCursor`], and an
/// [`EmitterErrorPolicy`]. All collaborators are taken by value so the
/// emitter is the unique writer of every observable side effect
/// (single-emitter invariant; section 2.3 of the design).
#[derive(Debug)]
pub struct DeleteEmitter<F: DeleteFs> {
    fs: F,
    stats: DeleteStats,
    plans: DeletePlanMap,
    cursor: DirTraversalCursor,
    policy: EmitterErrorPolicy,
    /// Read-only hardlink-cohort snapshot for the active INC_RECURSE
    /// segment. When `Some`, every successful delete dispatch records a
    /// cohort-tagged trace so callers can attach cohort information to
    /// itemize lines without re-statting. The dispatch itself is
    /// unchanged - matching upstream `delete.c:130-225`, every extras
    /// path is unlinked unconditionally and the kernel reconciles ref
    /// counts. The snapshot is wrapped in [`Arc`] so the same value can
    /// be shared with phase-1 workers.
    cohort_index: Option<Arc<CohortIndex>>,
    /// Sequence of cohort-tagged dispatches recorded during
    /// [`Self::emit_all`]. Each entry pairs the delete event with the
    /// cohort id (if the entry carried one) and the source-side ref
    /// count for that cohort at snapshot time. Populated only when the
    /// cohort index is attached; otherwise stays empty so the legacy
    /// code path pays no overhead.
    cohort_log: Vec<CohortDeleteRecord>,
    /// Accumulated non-fatal I/O error state. Bit 0 mirrors upstream's
    /// `IOERR_GENERAL`; the field is exposed via [`Self::io_error`] for
    /// callers that need to compute the final exit code.
    io_error: i32,
    /// Directory pulled from `cursor` whose plan was not yet published.
    /// Held across `emit_all` calls so the drain can resume once the plan
    /// arrives. `None` while the cursor is fully drained or not yet
    /// blocked.
    pending: Option<PathBuf>,
}

impl<F: DeleteFs> DeleteEmitter<F> {
    /// Builds an emitter with the default [`EmitterErrorPolicy`].
    #[must_use]
    pub fn new(fs: F, plans: DeletePlanMap, cursor: DirTraversalCursor) -> Self {
        Self::with_policy(fs, plans, cursor, EmitterErrorPolicy::default())
    }

    /// Builds an emitter with a caller-supplied [`EmitterErrorPolicy`].
    #[must_use]
    pub fn with_policy(
        fs: F,
        plans: DeletePlanMap,
        cursor: DirTraversalCursor,
        policy: EmitterErrorPolicy,
    ) -> Self {
        Self {
            fs,
            stats: DeleteStats::new(),
            plans,
            cursor,
            policy,
            cohort_index: None,
            cohort_log: Vec::new(),
            io_error: 0,
            pending: None,
        }
    }

    /// Builds an emitter that consults a hardlink-cohort snapshot for
    /// every dispatch.
    ///
    /// Wiring a [`CohortIndex`] does not change which paths get
    /// unlinked - upstream `delete.c:130-225` always issues
    /// `do_unlink`, and the kernel reconciles ref counts. The snapshot
    /// powers the emitter's cohort log (see [`Self::cohort_records`]),
    /// which downstream itemize formatting consumes to tag deletions
    /// with their leader cohort.
    #[must_use]
    pub fn with_cohort_index(
        fs: F,
        plans: DeletePlanMap,
        cursor: DirTraversalCursor,
        policy: EmitterErrorPolicy,
        cohort_index: Arc<CohortIndex>,
    ) -> Self {
        let mut emitter = Self::with_policy(fs, plans, cursor, policy);
        emitter.cohort_index = Some(cohort_index);
        emitter
    }

    /// Returns the recorded cohort-aware dispatch log.
    ///
    /// Empty when no [`CohortIndex`] is attached or when no delete
    /// dispatch has run yet. The slice is in dispatch order, matching
    /// the syscall order the emitter issued.
    #[must_use]
    pub fn cohort_records(&self) -> &[CohortDeleteRecord] {
        &self.cohort_log
    }

    /// Borrows the attached [`CohortIndex`], if any. Useful for tests
    /// that want to assert the emitter is consulting the snapshot the
    /// caller handed it.
    #[must_use]
    pub fn cohort_index(&self) -> Option<&Arc<CohortIndex>> {
        self.cohort_index.as_ref()
    }

    /// Returns the running deletion statistics. The counter is mutated
    /// only inside [`Self::emit_all`].
    #[must_use]
    pub fn stats(&self) -> DeleteStats {
        self.stats
    }

    /// Borrows the underlying [`DeleteFs`] dispatcher. Useful for tests
    /// that hold a [`RecordingDeleteFs`] and want to inspect events
    /// without dropping the emitter.
    #[must_use]
    pub fn fs(&self) -> &F {
        &self.fs
    }

    /// Consumes the emitter and returns the underlying [`DeleteFs`]
    /// dispatcher. Used by callers that need to inspect the recorded
    /// event sequence after the drain returns, since the emitter holds
    /// the dispatcher by value.
    #[must_use]
    pub fn into_fs(self) -> F {
        self.fs
    }

    /// Returns the accumulated `io_error` bitmask. Non-zero means at
    /// least one non-fatal I/O failure occurred during the drain and the
    /// caller should report exit code [`EMITTER_PARTIAL_EXIT_CODE`].
    #[must_use]
    pub fn io_error(&self) -> i32 {
        self.io_error
    }

    /// Maps the current `io_error` state to the upstream exit code the
    /// caller should surface when the drain completed without a fatal
    /// abort. Returns `0` on a clean run, `24` if every failure was a
    /// vanished-file race, and `23` for any other I/O error. Mirrors
    /// upstream `main.c::cleanup_and_exit` which prefers `RERR_VANISHED`
    /// only when no other error class was observed.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.io_error == 0 {
            0
        } else if self.io_error == IOERR_VANISHED_ONLY {
            EMITTER_VANISHED_EXIT_CODE
        } else {
            EMITTER_PARTIAL_EXIT_CODE
        }
    }

    /// Drains every ready directory in upstream traversal order, issuing
    /// one [`DeleteFs`] call per planned entry and incrementing the
    /// matching [`DeleteStats`] counter.
    ///
    /// Returns when the cursor exposes a directory whose plan has not
    /// been published yet (the parallel `compute_extras` worker is still
    /// running). The caller may invoke `emit_all` again once more plans
    /// have landed.
    ///
    /// # Errors
    ///
    /// Surfaces an [`io::Error`] only on a fatal classification (see
    /// [`Self::is_fatal_error`]) or when
    /// [`EmitterErrorPolicy::continue_on_error`] is `false` and a
    /// non-fatal failure occurs. Non-fatal failures under the default
    /// policy set [`Self::io_error`] and the drain continues, matching
    /// upstream `delete.c:178-207`.
    pub fn emit_all(&mut self) -> io::Result<()> {
        loop {
            let dir = match self.pending.take() {
                Some(dir) => dir,
                None => match self.cursor.next_ready() {
                    Some(dir) => dir,
                    None => return Ok(()),
                },
            };
            let Some(plan) = self.plans.take(&dir) else {
                // Plan for this directory has not landed yet. Park the
                // dir so a later `emit_all` call resumes from this point.
                self.pending = Some(dir);
                return Ok(());
            };
            self.drain_plan(&plan)?;
        }
    }

    /// Walks one published plan, dispatching each entry under the
    /// configured [`EmitterErrorPolicy`]. Used by both the top-level
    /// drain and the ENOTEMPTY recursive fallback so a nested directory
    /// gets the same continue-on-error semantics as a top-level one,
    /// matching upstream `delete_dir_contents` (`delete.c:86-109`) which
    /// iterates the dirlist and keeps going after each per-entry
    /// failure.
    fn drain_plan(&mut self, plan: &DeletePlan) -> io::Result<()> {
        for entry in &plan.extras {
            let full = plan.directory.join(&entry.name);
            self.run_entry(entry.kind, &full, entry.hardlink_cohort)?;
        }
        Ok(())
    }

    /// Issues one [`DeleteFs`] call, updates stats on success, and
    /// applies the error policy on failure. Fatal failures abort by
    /// returning `Err`; non-fatal failures under the default policy
    /// record `io_error` and return `Ok(())` so the caller's loop
    /// continues.
    fn run_entry(
        &mut self,
        kind: DeleteEntryKind,
        path: &Path,
        cohort: Option<HardlinkCohortId>,
    ) -> io::Result<()> {
        match self.dispatch(kind, path) {
            Ok(()) => {
                Self::increment_stat(&mut self.stats, kind);
                self.record_cohort_dispatch(path, kind, cohort);
                Ok(())
            }
            Err(err) => {
                if Self::is_fatal_error(&err) {
                    // Fatal: abort the drain. Upstream maps these to
                    // RERR_PARTIAL via `rsyserr(FERROR_XFER, ...)` plus
                    // `exit_cleanup` in `delete.c:201-205`.
                    return Err(err);
                }
                self.record_nonfatal(&err);
                if !self.policy.continue_on_error {
                    return Err(err);
                }
                Ok(())
            }
        }
    }

    /// Appends one [`CohortDeleteRecord`] to the cohort log when a
    /// [`CohortIndex`] is attached. The dispatch syscall itself has
    /// already succeeded; this is pure bookkeeping.
    fn record_cohort_dispatch(
        &mut self,
        path: &Path,
        kind: DeleteEntryKind,
        cohort: Option<HardlinkCohortId>,
    ) {
        let Some(index) = self.cohort_index.as_ref() else {
            return;
        };
        let surviving_source_refs = cohort
            .map(|c| index.surviving_refs_in_cohort(c))
            .unwrap_or(0);
        self.cohort_log.push(CohortDeleteRecord {
            path: path.to_path_buf(),
            kind,
            cohort,
            surviving_source_refs,
        });
    }

    /// Dispatches one planned entry. Directories route through
    /// [`Self::dispatch_dir`] so the `ENOTEMPTY` fallback can recurse via
    /// a nested plan or [`DeleteFs::remove_dir_all`].
    fn dispatch(&mut self, kind: DeleteEntryKind, path: &Path) -> io::Result<()> {
        match kind {
            DeleteEntryKind::File => self.fs.unlink_file(path),
            DeleteEntryKind::Dir => self.dispatch_dir(path),
            DeleteEntryKind::Symlink => self.fs.unlink_symlink(path),
            DeleteEntryKind::Device => self.fs.unlink_device(path),
            DeleteEntryKind::Special => self.fs.unlink_special(path),
        }
    }

    /// Handles a directory entry. Tries `rmdir` first (upstream
    /// `delete.c:161-163`); on [`io::ErrorKind::DirectoryNotEmpty`] takes
    /// the nested directory's published plan and drains it through the
    /// shared [`Self::drain_plan`] loop, or falls back to
    /// [`DeleteFs::remove_dir_all`] when no plan was published (upstream
    /// `delete.c:48-122 delete_dir_contents`). The retried `rmdir` after
    /// a successful drain matches `delete_item`'s second pass.
    fn dispatch_dir(&mut self, path: &Path) -> io::Result<()> {
        match self.fs.rmdir(path) {
            Ok(()) => Ok(()),
            Err(err) if is_not_empty(&err) => {
                if let Some(plan) = self.plans.take(path) {
                    self.drain_plan(&plan)?;
                    // Retry the rmdir now that the contents are gone.
                    self.fs.rmdir(path)
                } else {
                    self.fs.remove_dir_all(path)
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Records a non-fatal failure into `io_error`, honouring
    /// [`EmitterErrorPolicy::ignore_errors`]. Errors with
    /// [`io::ErrorKind::NotFound`] still flip the dedicated vanished bit
    /// so the caller can report exit code 24 when nothing else went
    /// wrong.
    fn record_nonfatal(&mut self, err: &io::Error) {
        if self.policy.ignore_errors {
            return;
        }
        if err.kind() == io::ErrorKind::NotFound {
            self.io_error |= IOERR_VANISHED_ONLY;
        } else {
            self.io_error |= IOERR_GENERAL;
        }
    }

    /// Classifies an error as fatal. Fatal classifications abort the
    /// drain and surface to the caller verbatim.
    ///
    /// Upstream treats `EPERM` and `EACCES` on the destination as
    /// fatal-class errors during the delete pass: they signal the
    /// receiver cannot make progress and the run should exit with
    /// `RERR_PARTIAL` immediately rather than spinning through the rest
    /// of the plan (see `delete.c:201-205` rsyserr + `cleanup_and_exit`).
    fn is_fatal_error(err: &io::Error) -> bool {
        matches!(err.kind(), io::ErrorKind::PermissionDenied)
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
}

/// `true` if the error reports the directory is not empty. Handles both
/// the stable [`io::ErrorKind::DirectoryNotEmpty`] and the raw
/// `ENOTEMPTY` errno path for older platforms.
fn is_not_empty(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::DirectoryNotEmpty {
        return true;
    }
    // ENOTEMPTY is 39 on Linux, 66 on BSD/macOS. Keep the check raw so
    // it works on any platform without a libc dep.
    matches!(err.raw_os_error(), Some(39) | Some(66))
}
