//! Single-threaded emitter for the parallel-deterministic delete pipeline.
//!
//! Scaffolds [`DeleteEmitter`], the drain task that consumes
//! [`DeletePlan`]s from a [`DeletePlanMap`] in the order dictated by
//! [`DirTraversalCursor`] (upstream depth-first traversal) and issues one
//! filesystem operation per planned entry through a [`DeleteFs`] trait.
//!
//! # Task scope (DDP-C1, #2259)
//!
//! This task lands:
//!
//! - The [`DeleteFs`] trait and its production implementation
//!   [`RealDeleteFs`].
//! - A [`RecordingDeleteFs`] test fake that captures the syscall sequence.
//! - The [`DeleteEmitter`] struct, its constructor, and the skeleton
//!   [`DeleteEmitter::emit_all`] loop that walks the cursor, takes the plan
//!   for each directory, and dispatches each entry to the [`DeleteFs`]
//!   trait while incrementing [`DeleteStats`].
//!
//! Out of scope (later tasks in the DDP-C series):
//!
//! - Live syscall dispatch refinements (DDP-C2, #2260).
//! - Error policy (`io_error & IOERR_GENERAL`, `--max-delete`, `ENOTEMPTY`)
//!   per upstream `delete.c:130-225` and `generator.c:272-298` (DDP-C3,
//!   #2261).
//! - Wiring through `MsgInfoSender` for `*deleting` itemize lines
//!   (DDP-C-series follow-ups).
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
//!   (`delete_item`): dispatch by `S_ISDIR` / `S_ISLNK` / `IS_DEVICE` /
//!   `IS_SPECIAL`, with `do_rmdir` for directories and `robust_unlink` for
//!   everything else.
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
//!   (`delete_in_dir`): reverse iteration over the sorted destination
//!   listing, one `delete_item` call per non-matched entry.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protocol::DeleteStats;

use super::{DeleteEntryKind, DeletePlanMap, DirTraversalCursor};

// ----------------------------------------------------------------------------
// DeleteFs trait and implementations.
// ----------------------------------------------------------------------------

/// Filesystem operations the emitter needs to issue a deletion.
///
/// The trait carves one method per upstream-distinguishable entry kind
/// (`delete.c:144-176`). Splitting `unlink_file` from `unlink_symlink` /
/// `unlink_device` / `unlink_special` lets unit tests assert the exact
/// dispatch table even though all four currently route to `unlink(2)` in the
/// production implementation. Directories use `rmdir(2)`.
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
}

/// Production [`DeleteFs`] implementation backed by `std::fs`.
///
/// All file-like kinds route to [`fs::remove_file`] (Unix `unlink(2)`,
/// Windows `DeleteFileW`). Directories route to [`fs::remove_dir`]
/// (`rmdir(2)`). This mirrors upstream `delete_item` (`delete.c:161-175`):
/// `do_rmdir` for `S_ISDIR`, `robust_unlink` for everything else.
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
}

/// Event captured by [`RecordingDeleteFs`] for each emitter dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteEvent {
    /// Path the emitter passed to [`DeleteFs`].
    pub path: PathBuf,
    /// Which trait method was invoked.
    pub kind: DeleteEntryKind,
}

/// Test fake that records every [`DeleteFs`] dispatch and never touches the
/// filesystem.
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
}

// ----------------------------------------------------------------------------
// Emitter.
// ----------------------------------------------------------------------------

/// Single-threaded drain task that issues deletions for one transfer.
///
/// Owns a [`DeleteFs`] dispatcher, a counter [`DeleteStats`], the published
/// [`DeletePlanMap`], and a [`DirTraversalCursor`]. All four are taken by
/// value so the emitter is the unique writer of every observable side
/// effect (single-emitter invariant; section 2.3 of the design).
pub struct DeleteEmitter<F: DeleteFs> {
    fs: F,
    stats: DeleteStats,
    plans: DeletePlanMap,
    cursor: DirTraversalCursor,
    /// Directory pulled from `cursor` whose plan was not yet published.
    /// Held across `emit_all` calls so the drain can resume once the plan
    /// arrives. `None` while the cursor is fully drained or not yet
    /// blocked.
    pending: Option<PathBuf>,
}

impl<F: DeleteFs> DeleteEmitter<F> {
    /// Builds an emitter from its three owned collaborators.
    #[must_use]
    pub fn new(fs: F, plans: DeletePlanMap, cursor: DirTraversalCursor) -> Self {
        Self {
            fs,
            stats: DeleteStats::new(),
            plans,
            cursor,
            pending: None,
        }
    }

    /// Returns the running deletion statistics. The counter is mutated only
    /// inside [`Self::emit_all`].
    #[must_use]
    pub fn stats(&self) -> DeleteStats {
        self.stats
    }

    /// Borrows the underlying [`DeleteFs`] dispatcher. Useful for tests that
    /// hold a [`RecordingDeleteFs`] and want to inspect events without
    /// dropping the emitter.
    #[must_use]
    pub fn fs(&self) -> &F {
        &self.fs
    }

    /// Drains every ready directory in upstream traversal order, issuing one
    /// [`DeleteFs`] call per planned entry and incrementing the matching
    /// [`DeleteStats`] counter.
    ///
    /// Returns when the cursor exposes a directory whose plan has not been
    /// published yet (the parallel `compute_extras` worker is still
    /// running). The caller may invoke `emit_all` again once more plans
    /// have landed.
    ///
    /// # Errors
    ///
    /// Surfaces the first [`io::Error`] returned by [`DeleteFs`]. Upstream's
    /// continue-on-failure policy (`delete.c:178-207`) is implemented in
    /// task DDP-C3 (#2261); the scaffold stops at the first error so test
    /// fakes can prove the dispatch is being exercised.
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
                // Plan for this directory has not landed yet. Park the dir
                // so a later `emit_all` call resumes from this point.
                self.pending = Some(dir);
                return Ok(());
            };
            for entry in &plan.extras {
                let full = plan.directory.join(&entry.name);
                self.dispatch(entry.kind, &full)?;
                Self::increment_stat(&mut self.stats, entry.kind);
            }
        }
    }

    fn dispatch(&self, kind: DeleteEntryKind, path: &Path) -> io::Result<()> {
        match kind {
            DeleteEntryKind::File => self.fs.unlink_file(path),
            DeleteEntryKind::Dir => self.fs.rmdir(path),
            DeleteEntryKind::Symlink => self.fs.unlink_symlink(path),
            DeleteEntryKind::Device => self.fs.unlink_device(path),
            DeleteEntryKind::Special => self.fs.unlink_special(path),
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

    #[test]
    fn empty_plan_map_returns_immediately() {
        // Cursor with no observations still emits its root once, but with
        // no plan published the emitter parks and returns cleanly without
        // touching the fs.
        let cursor = DirTraversalCursor::new(PathBuf::from("root"));
        let mut emitter =
            DeleteEmitter::new(RecordingDeleteFs::new(), DeletePlanMap::new(), cursor);
        emitter.emit_all().expect("empty drain succeeds");
        assert!(emitter.fs().events().is_empty());
        assert_eq!(emitter.stats(), DeleteStats::default());
    }

    #[test]
    fn dispatch_table_matches_planned_kind() {
        // One directory holds one entry per upstream kind so we observe the
        // full dispatch matrix (delete.c:144-176) in a single drain.
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
    }

    #[test]
    fn three_dirs_emit_in_cursor_order_within_plan_order() {
        // Three directories, each with two entries. The emitter must drain
        // them in cursor order (the upstream traversal order); within each
        // directory it must walk `extras` in the order the plan stores
        // them, which the design specifies as `compare_file_entries`
        // ascending then reversed (generator.c:320). The plans below are
        // already pre-reversed, so the expected sequence is exactly the
        // order they appear in the plan.
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
        // Pre-populate the root so it has no extras of its own, then
        // observe its three child directories in cursor order.
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
        // Cursor yields root then root/a, root/b, root/c. Only root, a,
        // and b have plans on the first drain. The emitter must walk root
        // and `a`, then park at `b` once it sees no plan published yet (it
        // would have processed b only if its plan were present). A later
        // publication of `b` and `c` plus a second `emit_all` must
        // complete the drain.
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

        // Publish the missing plans and resume. The parked directory
        // (root/b) drains first, then the cursor advances to root/c.
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
        // Smoke test that RealDeleteFs maps each kind to the expected
        // std::fs primitive. Symlink, device, and special kinds aren't
        // exercised here (Unix-only, root-required); they share the same
        // remove_file path as File so this still proves the wiring.
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
}
