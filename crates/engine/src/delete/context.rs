//! Receiver-driver glue between the flist segment consumer and the
//! parallel-deterministic-delete pipeline's phase-1 producers.
//!
//! [`DeleteContext`] is the shared handle the receiver threads through to
//! the per-segment hook. For each INC_RECURSE segment that lands on the
//! receiver, the hook calls [`DeleteContext::observe_segment_for_delete`]
//! with the segment's content directory and entries. The context computes
//! per-directory extras via [`compute_extras`], wraps them in a sorted
//! [`DeletePlan`], and publishes the plan into a shared [`DeletePlanMap`]
//! for the (not-yet-wired) emitter to drain.
//!
//! The same call also records the segment's child directories in a shared
//! [`DirTraversalCursor`] so the emitter can yield directories in upstream
//! traversal order without re-walking the flist.
//!
//! # Concurrency
//!
//! The plan map is already lock-free at the publisher boundary; the
//! traversal cursor is single-threaded by construction, so it lives behind
//! a [`Mutex`]. The lock is held only for the duration of one
//! `observe_segment` call (no I/O, no sorting), which matches the design
//! note in `traversal.rs` that the cursor is contention-tolerant in this
//! shape.
//!
//! # Scope
//!
//! This module deliberately does not unlink anything, mutate
//! `DeleteStats`, or emit itemize lines. Those live on the emitter side
//! and are wired in tasks DDP-E1-E5. Publishing plans into the map is a
//! pure observation: if no consumer is attached, the map fills and the
//! receiver finalizes normally on the existing batched-sweep code path.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use protocol::flist::FileEntry;

use super::extras::compute_extras;
use super::plan::DeletePlan;
use super::plan_map::DeletePlanMap;
use super::traversal::DirTraversalCursor;

/// Shared receiver-side handle that ties the flist segment consumer to
/// the parallel-deterministic-delete pipeline's plan map and traversal
/// cursor.
///
/// Construct one per transfer rooted at the destination directory, then
/// thread it through the receiver's INC_RECURSE segment hook. The
/// receiver calls [`Self::observe_segment_for_delete`] once per arriving
/// segment.
///
/// # Fields
///
/// - `plan_map` is shared with the (future) emitter thread.
/// - `cursor` is shared with the emitter to coordinate traversal order.
/// - `dest_root` is the destination root the receiver writes into; all
///   per-segment directory paths are resolved relative to it.
/// - `enabled` is the master switch. When `false`,
///   [`Self::observe_segment_for_delete`] is a no-op, so callers can
///   wire the context unconditionally and let it stay dormant when the
///   transfer is not in a delete mode.
#[derive(Debug)]
pub struct DeleteContext {
    /// Concurrent map keyed by destination-relative directory path that
    /// receives one [`DeletePlan`] per observed segment.
    pub plan_map: Arc<DeletePlanMap>,
    /// Cursor that records child directories per segment so the emitter
    /// can yield directories in upstream `f_name_cmp` ascending order.
    pub cursor: Mutex<DirTraversalCursor>,
    /// Absolute (or transfer-relative) destination root. Used to resolve
    /// per-segment relative directories into the paths that
    /// [`compute_extras`] passes to `read_dir`.
    pub dest_root: PathBuf,
    /// Master switch. `false` makes [`Self::observe_segment_for_delete`]
    /// a no-op so callers can wire the context unconditionally.
    pub enabled: bool,
}

impl DeleteContext {
    /// Constructs a new context rooted at `dest_root`.
    ///
    /// The traversal cursor is rooted at the empty relative path
    /// (matching the destination root itself, which upstream
    /// `delete_in_dir` visits first). When `enabled` is false, the
    /// context is a pass-through: workers may still call
    /// [`Self::observe_segment_for_delete`], but no plans land in the
    /// map.
    #[must_use]
    pub fn new(plan_map: Arc<DeletePlanMap>, dest_root: PathBuf, enabled: bool) -> Self {
        Self {
            plan_map,
            cursor: Mutex::new(DirTraversalCursor::new(PathBuf::new())),
            dest_root,
            enabled,
        }
    }

    /// Constructs a context whose traversal cursor is rooted at
    /// `cursor_root` rather than the empty path.
    ///
    /// Useful when the caller wants the emitter to begin its drain at a
    /// specific subtree (for example, when the transfer's source is a
    /// single directory below the destination root).
    #[must_use]
    pub fn with_cursor_root(
        plan_map: Arc<DeletePlanMap>,
        dest_root: PathBuf,
        cursor_root: PathBuf,
        enabled: bool,
    ) -> Self {
        Self {
            plan_map,
            cursor: Mutex::new(DirTraversalCursor::new(cursor_root)),
            dest_root,
            enabled,
        }
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
    /// 4. Inserts the plan into [`Self::plan_map`] keyed by `dir`.
    /// 5. Records the segment's children in [`Self::cursor`] via
    ///    [`DirTraversalCursor::observe_segment`].
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from [`compute_extras`] when the
    /// destination directory cannot be read. The receiver caller is
    /// expected to log + continue rather than abort the transfer (the
    /// existing batched-sweep path will still run), matching upstream's
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
        self.plan_map.insert(plan);

        self.cursor
            .lock()
            .expect("DeleteContext cursor mutex poisoned")
            .observe_segment(dir.to_path_buf(), entries);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::PathBuf;

    use protocol::flist::FileEntry;
    use tempfile::TempDir;

    use super::*;

    fn touch(dir: &Path, name: &str) {
        File::create(dir.join(name)).expect("touch");
    }

    fn flist_file(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    fn flist_dir(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    #[test]
    fn disabled_context_publishes_nothing() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "extra");
        let map = Arc::new(DeletePlanMap::new());
        let ctx = DeleteContext::new(Arc::clone(&map), dir.path().to_path_buf(), false);

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
        // dest layout:
        //   <root>/sub/a   <- in segment (kept)
        //   <root>/sub/b   <- extra (to delete)
        //   <root>/sub/c   <- in segment (kept)
        //   <root>/sub/d   <- extra (to delete)
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        for n in ["a", "b", "c", "d"] {
            touch(&sub, n);
        }

        let map = Arc::new(DeletePlanMap::new());
        let ctx = DeleteContext::new(Arc::clone(&map), dir.path().to_path_buf(), true);

        // Observe the root segment first so the cursor's depth-first
        // walk knows "sub" is a child of the (empty) root. Without
        // this, "sub/nested" is recorded but never reached during
        // emission.
        std::fs::create_dir_all(dir.path().join("placeholder")).unwrap();
        let root_segment = vec![flist_dir("sub")];
        ctx.observe_segment_for_delete(Path::new(""), &root_segment)
            .expect("root observe ok");

        let segment = vec![flist_file("a"), flist_file("c"), flist_dir("nested")];
        ctx.observe_segment_for_delete(Path::new("sub"), &segment)
            .expect("observe ok");

        assert!(map.contains(Path::new("sub")));
        let plan = map.take(Path::new("sub")).expect("plan present");
        assert!(plan.is_sorted());
        // After sort_by_name, plan-order is reverse of f_name_cmp
        // ascending, so b, d -> d, b.
        let names: Vec<&std::ffi::OsStr> = plan.extras.iter().map(|e| e.name.as_os_str()).collect();
        assert_eq!(
            names,
            vec![std::ffi::OsStr::new("d"), std::ffi::OsStr::new("b"),]
        );

        // Cursor should have recorded "sub/nested" as a child of "sub".
        let mut cursor = ctx.cursor.lock().unwrap();
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert!(seq.contains(&PathBuf::from("sub/nested")));
    }

    #[test]
    fn accumulates_plans_across_segments() {
        let root = TempDir::new().unwrap();
        for sub in ["s1", "s2", "s3"] {
            let p = root.path().join(sub);
            std::fs::create_dir(&p).unwrap();
            touch(&p, "keeper");
            touch(&p, "trash");
        }

        let map = Arc::new(DeletePlanMap::new());
        let ctx = DeleteContext::new(Arc::clone(&map), root.path().to_path_buf(), true);

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
        let ctx = DeleteContext::new(Arc::clone(&map), root.path().to_path_buf(), true);

        let err = ctx
            .observe_segment_for_delete(Path::new("does-not-exist"), &[])
            .expect_err("missing dir is an error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        // Nothing should have been published on failure.
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
        let ctx = DeleteContext::new(Arc::clone(&map), root.path().to_path_buf(), true);
        ctx.observe_segment_for_delete(Path::new(""), &[])
            .expect("observe ok");

        let plan = map.take(Path::new("")).expect("plan present");
        assert_eq!(plan.extras.len(), 2);
        assert!(plan.is_sorted());
    }
}
