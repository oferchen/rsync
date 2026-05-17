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

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use protocol::DeleteStats;

use super::cohort_index::CohortIndex;
use super::plan::HardlinkCohortId;
use super::{DeleteEntryKind, DeletePlan, DeletePlanMap, DirTraversalCursor};

/// Exit code for partial transfers caused by an I/O failure during the
/// delete pass. Mirrors upstream `errcode.h::RERR_PARTIAL` and
/// `core::exit_code::ExitCode::PartialTransfer`.
pub const EMITTER_PARTIAL_EXIT_CODE: i32 = 23;

/// Exit code reported when a destination entry vanished mid-pass. Mirrors
/// upstream `errcode.h::RERR_VANISHED` and `core::exit_code::ExitCode::Vanished`.
pub const EMITTER_VANISHED_EXIT_CODE: i32 = 24;

/// Filesystem operations the emitter needs to issue a deletion.
///
/// The trait carves one method per upstream-distinguishable entry kind
/// (`delete.c:144-176`). Splitting `unlink_file` from `unlink_symlink` /
/// `unlink_device` / `unlink_special` lets unit tests assert the exact
/// dispatch table even though all four currently route to `unlink(2)` in
/// the production implementation. Directories use `rmdir(2)`; the
/// recursive [`Self::remove_dir_all`] hook mirrors upstream's
/// `delete_dir_contents` fallback when a directory cannot be emptied via
/// its own published plan.
///
/// All methods take `&self` so a single [`DeleteFs`] value can be shared
/// across the emitter and any future helpers. The production impl is
/// stateless; the test fake holds a `Mutex` because the recording is
/// observable from the test thread after `emit_all` returns.
pub trait DeleteFs {
    /// Unlinks a regular file.
    fn unlink_file(&self, path: &Path) -> io::Result<()>;

    /// Removes an empty directory.
    fn rmdir(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a symbolic link.
    fn unlink_symlink(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a block or character device node.
    fn unlink_device(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a FIFO or socket.
    fn unlink_special(&self, path: &Path) -> io::Result<()>;

    /// Recursively removes a directory and everything beneath it.
    ///
    /// Invoked by the emitter when [`Self::rmdir`] returns
    /// [`io::ErrorKind::DirectoryNotEmpty`] and no nested
    /// [`super::DeletePlan`] has been published for the offending child
    /// (upstream `delete.c:48-122 delete_dir_contents`).
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
}

/// Production [`DeleteFs`] implementation backed by `std::fs`.
///
/// All file-like kinds route to [`fs::remove_file`] (Unix `unlink(2)`,
/// Windows `DeleteFileW`). Directories route to [`fs::remove_dir`]
/// (`rmdir(2)`); the recursive fallback routes to [`fs::remove_dir_all`]
/// to match upstream `delete_dir_contents`. This mirrors upstream
/// `delete_item` (`delete.c:161-175`): `do_rmdir` for `S_ISDIR`,
/// `robust_unlink` for everything else.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealDeleteFs;

impl DeleteFs for RealDeleteFs {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }
}

/// Blanket impl so a shared reference behaves like the owned value. Lets
/// callers reuse a single [`RealDeleteFs`] across many emitter drains
/// without cloning, and matches the `&self` shape of every trait method.
impl<F: DeleteFs + ?Sized> DeleteFs for &F {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_file(path)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        (*self).rmdir(path)
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_symlink(path)
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_device(path)
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_special(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        (*self).remove_dir_all(path)
    }
}

/// Event captured by [`RecordingDeleteFs`] for each emitter dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteEvent {
    /// Path the emitter passed to [`DeleteFs`].
    pub path: PathBuf,
    /// Which trait method was invoked.
    pub kind: DeleteEntryKind,
}

/// Test fake that records every [`DeleteFs`] dispatch and never touches
/// the filesystem.
///
/// Used by the emitter unit tests to assert ordering invariants without
/// staging real files. The recorded sequence is the ground truth for the
/// "syscall order matches upstream" check that section 9.1 of the design
/// elevates to a release-gating interop test.
#[derive(Debug, Default)]
pub struct RecordingDeleteFs {
    events: Mutex<Vec<DeleteEvent>>,
}

impl RecordingDeleteFs {
    /// Creates an empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of the recorded events in dispatch order.
    #[must_use]
    pub fn events(&self) -> Vec<DeleteEvent> {
        self.events.lock().expect("recorder mutex poisoned").clone()
    }

    fn record(&self, path: &Path, kind: DeleteEntryKind) {
        self.events
            .lock()
            .expect("recorder mutex poisoned")
            .push(DeleteEvent {
                path: path.to_path_buf(),
                kind,
            });
    }
}

impl DeleteFs for RecordingDeleteFs {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::File);
        Ok(())
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Dir);
        Ok(())
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Symlink);
        Ok(())
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Device);
        Ok(())
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Special);
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        // Mirror upstream's recursive peel as a single Dir event so the
        // unit tests can assert "the emitter fell back to recursion for
        // this path".
        self.record(path, DeleteEntryKind::Dir);
        Ok(())
    }
}

/// Policy controlling how the emitter reacts to per-entry I/O failures.
///
/// Mirrors upstream rsync's `--ignore-errors` and continue-on-error
/// behaviour (`delete.c:178-207`). The two booleans are orthogonal:
///
/// - `ignore_errors`: when `true`, non-fatal failures are logged but the
///   shared `io_error` flag is NOT set. Matches upstream `--ignore-errors`
///   which suppresses the `IOERR_GENERAL` bit so the run can still exit 0.
/// - `continue_on_error`: when `true`, non-fatal failures do not abort the
///   drain - the emitter records the error in `io_error` (unless
///   suppressed by `ignore_errors`) and moves on to the next entry. When
///   `false`, the first non-fatal failure also stops the drain.
///
/// Fatal classifications (see [`DeleteEmitter::is_fatal_error`]) always
/// abort the drain regardless of these flags so the caller can surface
/// the failure with a non-zero exit code.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct EmitterErrorPolicy {
    /// Suppress the `io_error` flag for non-fatal failures.
    pub ignore_errors: bool,
    /// Keep draining after a non-fatal failure.
    pub continue_on_error: bool,
}

impl Default for EmitterErrorPolicy {
    /// Upstream's default: surface non-fatal errors via `io_error` but
    /// keep going. Matches `delete.c:178-207`: errors flip the flag and
    /// the loop in `delete_dir_contents` continues to the next entry.
    fn default() -> Self {
        Self {
            ignore_errors: false,
            continue_on_error: true,
        }
    }
}

/// Single-threaded drain task that issues deletions for one transfer.
///
/// Owns a [`DeleteFs`] dispatcher, a counter [`DeleteStats`], the
/// published [`DeletePlanMap`], a [`DirTraversalCursor`], and an
/// [`EmitterErrorPolicy`]. All collaborators are taken by value so the
/// emitter is the unique writer of every observable side effect
/// (single-emitter invariant; section 2.3 of the design).
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

/// One cohort-aware delete dispatch record.
///
/// Produced by the emitter when a [`super::DeleteEntry`] carries a
/// [`HardlinkCohortId`] and a [`CohortIndex`] is attached. The record
/// pairs the destination path with the cohort tag and the source-side
/// ref count for the cohort at snapshot time, giving callers enough
/// information to format an upstream-style itemize line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortDeleteRecord {
    /// Destination path the emitter dispatched against.
    pub path: PathBuf,
    /// Entry kind that drove the dispatch.
    pub kind: DeleteEntryKind,
    /// Cohort the entry belonged to (`None` if the destination was not
    /// part of any tracked cohort even though the index was attached).
    pub cohort: Option<HardlinkCohortId>,
    /// Source-side ref count for the cohort at snapshot time, or `0`
    /// when no cohort tag was present. Mirrors upstream's
    /// `match_hard_links` view of "how many links the upstream sent for
    /// this cohort".
    pub surviving_source_refs: u32,
}

/// Upstream `IOERR_GENERAL`: the only bit the delete pass currently sets.
const IOERR_GENERAL: i32 = 1;

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

/// Sentinel bit set when the only failure observed was a vanished
/// destination entry ([`io::ErrorKind::NotFound`]). Distinct from
/// `IOERR_GENERAL` so the caller can map a vanished-only run to exit
/// code 24 instead of 23.
const IOERR_VANISHED_ONLY: i32 = 1 << 1;

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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use protocol::flist::FileEntry;

    use super::super::{DeleteEntry, DeletePlan};
    use super::*;

    fn entry(name: &str, kind: DeleteEntryKind) -> DeleteEntry {
        DeleteEntry::new(OsString::from(name), kind)
    }

    fn plan(dir: &str, entries: Vec<DeleteEntry>) -> DeletePlan {
        DeletePlan::from_extras(PathBuf::from(dir), entries)
    }

    fn dir_child(parent: &str, name: &str) -> FileEntry {
        let path = if parent.is_empty() {
            PathBuf::from(name)
        } else {
            PathBuf::from(parent).join(name)
        };
        FileEntry::new_directory(path, 0o755)
    }

    /// Failure plan: for each (path, errno) pair, the next call against
    /// that path returns the matching error before falling back to the
    /// recording behaviour.
    #[derive(Default)]
    struct ScriptedDeleteFs {
        inner: RecordingDeleteFs,
        rules: Mutex<Vec<(PathBuf, io::ErrorKind)>>,
    }

    impl ScriptedDeleteFs {
        fn new() -> Self {
            Self::default()
        }

        fn fail(self, path: &str, kind: io::ErrorKind) -> Self {
            self.rules
                .lock()
                .expect("rules mutex")
                .push((PathBuf::from(path), kind));
            self
        }

        fn events(&self) -> Vec<DeleteEvent> {
            self.inner.events()
        }

        fn maybe_fail(&self, path: &Path) -> Option<io::Error> {
            let mut rules = self.rules.lock().expect("rules mutex");
            let position = rules.iter().position(|(p, _)| p == path)?;
            let (_, kind) = rules.remove(position);
            Some(io::Error::new(kind, "scripted failure"))
        }
    }

    impl DeleteFs for ScriptedDeleteFs {
        fn unlink_file(&self, path: &Path) -> io::Result<()> {
            if let Some(err) = self.maybe_fail(path) {
                return Err(err);
            }
            self.inner.unlink_file(path)
        }

        fn rmdir(&self, path: &Path) -> io::Result<()> {
            if let Some(err) = self.maybe_fail(path) {
                return Err(err);
            }
            self.inner.rmdir(path)
        }

        fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
            if let Some(err) = self.maybe_fail(path) {
                return Err(err);
            }
            self.inner.unlink_symlink(path)
        }

        fn unlink_device(&self, path: &Path) -> io::Result<()> {
            if let Some(err) = self.maybe_fail(path) {
                return Err(err);
            }
            self.inner.unlink_device(path)
        }

        fn unlink_special(&self, path: &Path) -> io::Result<()> {
            if let Some(err) = self.maybe_fail(path) {
                return Err(err);
            }
            self.inner.unlink_special(path)
        }

        fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
            if let Some(err) = self.maybe_fail(path) {
                return Err(err);
            }
            self.inner.remove_dir_all(path)
        }
    }

    #[test]
    fn empty_plan_map_returns_immediately() {
        let cursor = DirTraversalCursor::new(PathBuf::from("root"));
        let mut emitter =
            DeleteEmitter::new(RecordingDeleteFs::new(), DeletePlanMap::new(), cursor);
        emitter.emit_all().expect("empty drain succeeds");
        assert!(emitter.fs().events().is_empty());
        assert_eq!(emitter.stats(), DeleteStats::default());
        assert_eq!(emitter.io_error(), 0);
        assert_eq!(emitter.exit_code(), 0);
    }

    #[test]
    fn dispatch_table_matches_planned_kind() {
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("file", DeleteEntryKind::File),
                entry("dir", DeleteEntryKind::Dir),
                entry("link", DeleteEntryKind::Symlink),
                entry("dev", DeleteEntryKind::Device),
                entry("fifo", DeleteEntryKind::Special),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        let events = emitter.fs().events();
        assert_eq!(
            events,
            vec![
                DeleteEvent {
                    path: PathBuf::from("d/file"),
                    kind: DeleteEntryKind::File,
                },
                DeleteEvent {
                    path: PathBuf::from("d/dir"),
                    kind: DeleteEntryKind::Dir,
                },
                DeleteEvent {
                    path: PathBuf::from("d/link"),
                    kind: DeleteEntryKind::Symlink,
                },
                DeleteEvent {
                    path: PathBuf::from("d/dev"),
                    kind: DeleteEntryKind::Device,
                },
                DeleteEvent {
                    path: PathBuf::from("d/fifo"),
                    kind: DeleteEntryKind::Special,
                },
            ],
        );
        let stats = emitter.stats();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.dirs, 1);
        assert_eq!(stats.symlinks, 1);
        assert_eq!(stats.devices, 1);
        assert_eq!(stats.specials, 1);
        assert_eq!(stats.total(), 5);
        assert_eq!(emitter.io_error(), 0);
    }

    #[test]
    fn three_dirs_emit_in_cursor_order_within_plan_order() {
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "root/a",
            vec![
                entry("a_two", DeleteEntryKind::File),
                entry("a_one", DeleteEntryKind::Symlink),
            ],
        ));
        plans.insert(plan(
            "root/b",
            vec![
                entry("b_two", DeleteEntryKind::Dir),
                entry("b_one", DeleteEntryKind::File),
            ],
        ));
        plans.insert(plan(
            "root/c",
            vec![
                entry("c_two", DeleteEntryKind::Special),
                entry("c_one", DeleteEntryKind::Device),
            ],
        ));
        plans.insert(plan("root", vec![]));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment(
            PathBuf::from("root"),
            &[
                dir_child("root", "a"),
                dir_child("root", "b"),
                dir_child("root", "c"),
            ],
        );

        let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        let expected: Vec<DeleteEvent> = [
            ("root/a/a_two", DeleteEntryKind::File),
            ("root/a/a_one", DeleteEntryKind::Symlink),
            ("root/b/b_two", DeleteEntryKind::Dir),
            ("root/b/b_one", DeleteEntryKind::File),
            ("root/c/c_two", DeleteEntryKind::Special),
            ("root/c/c_one", DeleteEntryKind::Device),
        ]
        .iter()
        .map(|(p, k)| DeleteEvent {
            path: PathBuf::from(*p),
            kind: *k,
        })
        .collect();
        assert_eq!(emitter.fs().events(), expected);
        let stats = emitter.stats();
        assert_eq!(stats.total(), 6);
    }

    #[test]
    fn cursor_outpaces_plans_parks_at_gap_and_resumes() {
        let plans = DeletePlanMap::new();
        plans.insert(plan("root", vec![]));
        plans.insert(plan("root/a", vec![entry("x", DeleteEntryKind::File)]));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment(
            PathBuf::from("root"),
            &[
                dir_child("root", "a"),
                dir_child("root", "b"),
                dir_child("root", "c"),
            ],
        );

        let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
        emitter
            .emit_all()
            .expect("first drain succeeds up to the gap");
        assert_eq!(
            emitter
                .fs()
                .events()
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("root/a/x")],
        );

        emitter
            .plans
            .insert(plan("root/b", vec![entry("y", DeleteEntryKind::Symlink)]));
        emitter
            .plans
            .insert(plan("root/c", vec![entry("z", DeleteEntryKind::Dir)]));
        emitter.emit_all().expect("resume succeeds");
        let tail_paths: Vec<PathBuf> = emitter
            .fs()
            .events()
            .iter()
            .skip(1)
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            tail_paths,
            vec![PathBuf::from("root/b/y"), PathBuf::from("root/c/z")],
        );
        let stats = emitter.stats();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.symlinks, 1);
        assert_eq!(stats.dirs, 1);
    }

    #[test]
    fn real_delete_fs_round_trip_on_tempdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("f");
        std::fs::write(&file, b"x").expect("write file");
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).expect("mkdir");

        let fs = RealDeleteFs;
        fs.unlink_file(&file).expect("unlink_file");
        fs.rmdir(&dir).expect("rmdir");

        assert!(!file.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn mixed_kind_plan_preserves_input_dispatch_order() {
        // A single plan that touches every upstream-distinguishable kind
        // in a non-sorted order must dispatch in exactly the order
        // entries appear in `plan.extras` (the order phase-1 already
        // froze via `sort_by_name`). This is the load-bearing
        // upstream-parity invariant from section 9.1.
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("a-file", DeleteEntryKind::File),
                entry("b-dir", DeleteEntryKind::Dir),
                entry("c-link", DeleteEntryKind::Symlink),
                entry("d-dev", DeleteEntryKind::Device),
                entry("e-fifo", DeleteEntryKind::Special),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        let kinds: Vec<DeleteEntryKind> = emitter.fs().events().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DeleteEntryKind::File,
                DeleteEntryKind::Dir,
                DeleteEntryKind::Symlink,
                DeleteEntryKind::Device,
                DeleteEntryKind::Special,
            ],
        );
    }

    #[test]
    fn non_empty_dir_recurses_via_nested_plan() {
        // The directory `d/sub` has a published nested plan with two
        // entries. The parent plan deletes `d/sub` as a Dir; the
        // emitter's first rmdir attempt yields ENOTEMPTY, so the
        // emitter must drain the nested plan (in plan order) and retry
        // rmdir, mirroring upstream `delete_dir_contents` +
        // `delete_item` retry (`delete.c:48-122`, `:161-163`).
        let fs = ScriptedDeleteFs::new().fail("d/sub", io::ErrorKind::DirectoryNotEmpty);
        let plans = DeletePlanMap::new();
        plans.insert(plan("d", vec![entry("sub", DeleteEntryKind::Dir)]));
        plans.insert(plan(
            "d/sub",
            vec![
                entry("inner-file", DeleteEntryKind::File),
                entry("inner-link", DeleteEntryKind::Symlink),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(fs, plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        // Expected sequence: rmdir(sub) failed and was never recorded,
        // then inner-file unlink, inner-link unlink, then the retried
        // rmdir(sub) succeeded.
        let events = emitter.fs().events();
        assert_eq!(
            events,
            vec![
                DeleteEvent {
                    path: PathBuf::from("d/sub/inner-file"),
                    kind: DeleteEntryKind::File,
                },
                DeleteEvent {
                    path: PathBuf::from("d/sub/inner-link"),
                    kind: DeleteEntryKind::Symlink,
                },
                DeleteEvent {
                    path: PathBuf::from("d/sub"),
                    kind: DeleteEntryKind::Dir,
                },
            ],
        );
        assert_eq!(emitter.io_error(), 0);
        assert_eq!(emitter.stats().dirs, 1);
    }

    #[test]
    fn non_empty_dir_without_plan_falls_back_to_remove_dir_all() {
        // When `d/orphan` has no published plan, ENOTEMPTY must route
        // through `DeleteFs::remove_dir_all`, mirroring upstream's
        // `delete_dir_contents` peel.
        let fs = ScriptedDeleteFs::new().fail("d/orphan", io::ErrorKind::DirectoryNotEmpty);
        let plans = DeletePlanMap::new();
        plans.insert(plan("d", vec![entry("orphan", DeleteEntryKind::Dir)]));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(fs, plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        // The recorder logs `remove_dir_all` as a single Dir event on
        // the directory path.
        assert_eq!(
            emitter.fs().events(),
            vec![DeleteEvent {
                path: PathBuf::from("d/orphan"),
                kind: DeleteEntryKind::Dir,
            }],
        );
        assert_eq!(emitter.io_error(), 0);
        assert_eq!(emitter.stats().dirs, 1);
    }

    #[test]
    fn nonfatal_error_in_middle_keeps_draining_and_sets_io_error() {
        // The middle entry fails with EBUSY (Other). Continue policy is
        // on by default, so subsequent entries must still process and
        // io_error must reflect IOERR_GENERAL.
        let fs = ScriptedDeleteFs::new().fail("d/middle", io::ErrorKind::Other);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("first", DeleteEntryKind::File),
                entry("middle", DeleteEntryKind::File),
                entry("last", DeleteEntryKind::File),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(fs, plans, cursor);
        emitter.emit_all().expect("drain succeeds on non-fatal err");

        let paths: Vec<PathBuf> = emitter
            .fs()
            .events()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        // `middle` was never recorded because it errored before the
        // recording branch. `first` and `last` both succeeded.
        assert_eq!(
            paths,
            vec![PathBuf::from("d/first"), PathBuf::from("d/last")],
        );
        assert_eq!(emitter.io_error(), IOERR_GENERAL);
        assert_eq!(emitter.exit_code(), EMITTER_PARTIAL_EXIT_CODE);
        // Only successful entries bump the stats.
        assert_eq!(emitter.stats().files, 2);
    }

    #[test]
    fn fatal_eperm_aborts_drain_and_returns_err() {
        // EPERM on the destination is fatal under upstream's policy:
        // the drain must stop and the caller must surface the error.
        let fs = ScriptedDeleteFs::new().fail("d/second", io::ErrorKind::PermissionDenied);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("first", DeleteEntryKind::File),
                entry("second", DeleteEntryKind::File),
                entry("third", DeleteEntryKind::File),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(fs, plans, cursor);
        let err = emitter
            .emit_all()
            .expect_err("fatal classification aborts drain");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        let paths: Vec<PathBuf> = emitter
            .fs()
            .events()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        // `third` is never reached because the drain aborted.
        assert_eq!(paths, vec![PathBuf::from("d/first")]);
        assert_eq!(emitter.stats().files, 1);
    }

    #[test]
    fn ignore_errors_suppresses_io_error_flag() {
        // With ignore_errors=true the drain still continues but
        // io_error stays zero - matching upstream `--ignore-errors`.
        let fs = ScriptedDeleteFs::new().fail("d/bad", io::ErrorKind::Other);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("ok", DeleteEntryKind::File),
                entry("bad", DeleteEntryKind::File),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let policy = EmitterErrorPolicy {
            ignore_errors: true,
            continue_on_error: true,
        };
        let mut emitter = DeleteEmitter::with_policy(fs, plans, cursor, policy);
        emitter.emit_all().expect("drain succeeds");

        assert_eq!(emitter.io_error(), 0);
        assert_eq!(emitter.exit_code(), 0);
        assert_eq!(emitter.stats().files, 1);
    }

    #[test]
    fn vanished_only_maps_to_exit_24() {
        // NotFound on a destination entry is a vanished-race; when it's
        // the sole failure, the run must report exit code 24
        // (RERR_VANISHED), not 23.
        let fs = ScriptedDeleteFs::new().fail("d/gone", io::ErrorKind::NotFound);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("here", DeleteEntryKind::File),
                entry("gone", DeleteEntryKind::File),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(fs, plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        assert_eq!(emitter.io_error(), IOERR_VANISHED_ONLY);
        assert_eq!(emitter.exit_code(), EMITTER_VANISHED_EXIT_CODE);
        assert_eq!(emitter.stats().files, 1);
    }

    #[test]
    fn continue_off_aborts_on_first_nonfatal_error() {
        // continue_on_error=false stops at the first non-fatal failure
        // and surfaces the error to the caller.
        let fs = ScriptedDeleteFs::new().fail("d/two", io::ErrorKind::Other);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                entry("one", DeleteEntryKind::File),
                entry("two", DeleteEntryKind::File),
                entry("three", DeleteEntryKind::File),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let policy = EmitterErrorPolicy {
            ignore_errors: false,
            continue_on_error: false,
        };
        let mut emitter = DeleteEmitter::with_policy(fs, plans, cursor, policy);
        emitter
            .emit_all()
            .expect_err("non-fatal failure aborts when continue is off");

        let paths: Vec<PathBuf> = emitter
            .fs()
            .events()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(paths, vec![PathBuf::from("d/one")]);
        assert_eq!(emitter.io_error(), IOERR_GENERAL);
    }

    #[test]
    fn fully_drained_plan_map_yields_clean_ok() {
        // Plan-map exhaustion: after one successful drain the cursor is
        // empty, a second emit_all is a no-op that returns Ok(()) with
        // no new events and no io_error.
        let plans = DeletePlanMap::new();
        plans.insert(plan("d", vec![entry("x", DeleteEntryKind::File)]));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
        emitter.emit_all().expect("first drain succeeds");
        let after_first = emitter.fs().events().len();
        emitter.emit_all().expect("second drain is a no-op");
        assert_eq!(emitter.fs().events().len(), after_first);
        assert_eq!(emitter.io_error(), 0);
        assert_eq!(emitter.exit_code(), 0);
    }

    // Per `docs/design/hardlink-delete-audit.md` Option A and upstream
    // `delete.c:130-225`, the emitter still unlinks every destination
    // path even when the cohort retains other refs (the kernel
    // reconciles ref counts). The snapshot powers cohort-aware itemize
    // bookkeeping via `cohort_records`, not the unlink decision itself.

    fn tagged_entry(name: &str, kind: DeleteEntryKind, cohort: HardlinkCohortId) -> DeleteEntry {
        DeleteEntry::with_cohort(OsString::from(name), kind, cohort)
    }

    /// Builds a `CohortIndex` from a synthetic segment with `dest_count`
    /// source-side refs sharing one cohort. The leader's name is
    /// `leader`; followers are `member0`, `member1`, etc.
    fn cohort_index_for(leader: &str, member_count: usize) -> Arc<CohortIndex> {
        let mut entries = Vec::with_capacity(1 + member_count);
        let mut head = FileEntry::new_file(PathBuf::from(leader), 0, 0o644);
        head.set_hardlink_idx(u32::MAX);
        entries.push(head);
        for i in 0..member_count {
            let mut e = FileEntry::new_file(PathBuf::from(format!("member{i}")), 0, 0o644);
            e.set_hardlink_idx(0);
            entries.push(e);
        }
        CohortIndex::build_from_flist_segment(&entries)
    }

    #[test]
    fn emitter_with_cohort_index_records_cohort_per_dispatch() {
        // The destination has three refs in one cohort (`leader`,
        // `member0`, `member1`); the source has two (`leader` and
        // `member0`). Upstream's policy is to unlink all three dest
        // paths; the cohort log must reflect that with cohort tags on
        // the two known members and no tag on the third.
        let cohort = HardlinkCohortId::new(0);
        let index = cohort_index_for("leader", 1); // source = leader + member0
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![
                tagged_entry("leader", DeleteEntryKind::File, cohort),
                tagged_entry("member0", DeleteEntryKind::File, cohort),
                entry("member1", DeleteEntryKind::File),
            ],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::with_cohort_index(
            RecordingDeleteFs::new(),
            plans,
            cursor,
            EmitterErrorPolicy::default(),
            Arc::clone(&index),
        );
        emitter.emit_all().expect("drain succeeds");

        // The kernel-policy invariant: every dest path was unlinked
        // (matches upstream `delete.c:130-225`).
        let events = emitter.fs().events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].path, PathBuf::from("d/leader"));
        assert_eq!(events[1].path, PathBuf::from("d/member0"));
        assert_eq!(events[2].path, PathBuf::from("d/member1"));

        // The cohort log records the per-dispatch cohort tag.
        let records = emitter.cohort_records();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].cohort, Some(cohort));
        assert_eq!(records[0].surviving_source_refs, 2);
        assert_eq!(records[1].cohort, Some(cohort));
        assert_eq!(records[1].surviving_source_refs, 2);
        // The orphan-on-the-dest gets no cohort tag.
        assert_eq!(records[2].cohort, None);
        assert_eq!(records[2].surviving_source_refs, 0);
    }

    #[test]
    fn emitter_without_cohort_index_records_no_cohort_log() {
        // Baseline: with no CohortIndex attached, the cohort log stays
        // empty even when the plan carries cohort tags. This keeps the
        // legacy hot path zero-overhead.
        let cohort = HardlinkCohortId::new(7);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![tagged_entry("x", DeleteEntryKind::File, cohort)],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
        emitter.emit_all().expect("drain succeeds");

        assert!(emitter.cohort_index().is_none());
        assert!(emitter.cohort_records().is_empty());
        assert_eq!(emitter.stats().files, 1);
    }

    #[test]
    fn emitter_cohort_index_does_not_change_dispatch_order() {
        // Attaching a CohortIndex must not perturb the syscall order -
        // the cohort log is observer-only. The dispatch must match
        // exactly what the no-cohort variant produces.
        let cohort = HardlinkCohortId::new(0);
        let index = cohort_index_for("leader", 2);

        let baseline_plans = DeletePlanMap::new();
        baseline_plans.insert(plan(
            "d",
            vec![
                entry("leader", DeleteEntryKind::File),
                entry("member0", DeleteEntryKind::File),
                entry("member1", DeleteEntryKind::File),
            ],
        ));
        let mut baseline_cursor = DirTraversalCursor::new(PathBuf::from("d"));
        baseline_cursor.observe_segment(PathBuf::from("d"), &[]);
        let mut baseline_emitter =
            DeleteEmitter::new(RecordingDeleteFs::new(), baseline_plans, baseline_cursor);
        baseline_emitter.emit_all().unwrap();
        let baseline_events = baseline_emitter.fs().events();

        let cohort_plans = DeletePlanMap::new();
        cohort_plans.insert(plan(
            "d",
            vec![
                tagged_entry("leader", DeleteEntryKind::File, cohort),
                tagged_entry("member0", DeleteEntryKind::File, cohort),
                tagged_entry("member1", DeleteEntryKind::File, cohort),
            ],
        ));
        let mut cohort_cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cohort_cursor.observe_segment(PathBuf::from("d"), &[]);
        let mut cohort_emitter = DeleteEmitter::with_cohort_index(
            RecordingDeleteFs::new(),
            cohort_plans,
            cohort_cursor,
            EmitterErrorPolicy::default(),
            index,
        );
        cohort_emitter.emit_all().unwrap();
        assert_eq!(cohort_emitter.fs().events(), baseline_events);
    }

    #[test]
    fn cohort_log_skips_failed_dispatch() {
        // A failed dispatch must NOT add a cohort record - the cohort
        // log is meant to mirror successful syscalls (so it matches the
        // emitter's stats counter, which is also success-only).
        let cohort = HardlinkCohortId::new(0);
        let index = cohort_index_for("leader", 0); // single-ref source
        let fs = ScriptedDeleteFs::new().fail("d/leader", io::ErrorKind::Other);
        let plans = DeletePlanMap::new();
        plans.insert(plan(
            "d",
            vec![tagged_entry("leader", DeleteEntryKind::File, cohort)],
        ));
        let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
        cursor.observe_segment(PathBuf::from("d"), &[]);

        let mut emitter = DeleteEmitter::with_cohort_index(
            fs,
            plans,
            cursor,
            EmitterErrorPolicy::default(),
            index,
        );
        emitter
            .emit_all()
            .expect("non-fatal failure keeps draining");

        assert!(
            emitter.cohort_records().is_empty(),
            "failed dispatch must not appear in cohort log",
        );
        assert_eq!(emitter.io_error(), IOERR_GENERAL);
    }
}
