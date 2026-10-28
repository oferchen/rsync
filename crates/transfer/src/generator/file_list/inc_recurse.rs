//! INC_RECURSE file list partitioning for the generator role.
//!
//! Partitions the sorted file list into per-directory segments for
//! incremental recursion sending. Top-level entries are placed first,
//! followed by sub-directory entries in depth-first order.
//!
//! # dir_ndx alignment with upstream receiver
//!
//! The upstream receiver (`flist.c:recv_file_list()`) builds `dir_flist` by
//! appending every directory it encounters, in reception order:
//! 1. All dirs from the initial list (including ".") in sorted order
//! 2. For each sub-list (received in depth-first order): dirs in that sub-list
//!
//! Our `parent_dir_ndx` values in `PendingSegment` must match these indices
//! exactly, or the receiver's dirname validation at `flist.c:3170-3177` will
//! reject entries with "ABORTING due to invalid path from sender".
//!
//! # Upstream Reference
//!
//! - `flist.c:add_dirs_to_tree()` - organizes dirs into traversal tree
//! - `flist.c:send_extra_file_list()` - sends one segment per directory
//! - `flist.c:recv_file_list()` line 2643 - appends dirs to `dir_flist`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use logging::debug_log;
use protocol::flist::FileEntry;

use super::super::io_error_flags;
use super::super::protocol_io::SenderDiagnostic;
use super::super::{GeneratorContext, LazyFlistProducer, PendingSegment};

impl GeneratorContext {
    /// Partitions the sorted file list into segments for incremental recursion.
    ///
    /// Reorders `file_list` and `source_bases` so that initial (top-level)
    /// entries come first, then sub-directory entries in depth-first order. This
    /// makes NDX values correspond directly to indices in the reordered list.
    ///
    /// An entry whose parent directory is missing from the list cannot be
    /// segmented; it is reported as a transfer error rather than emitted
    /// silently, because the resulting initial-list row carries a non-empty
    /// dirname and a conformant receiver aborts on it (`flist.c:3491-3506`).
    pub(in crate::generator) fn partition_file_list_for_inc_recurse(&mut self) {
        if !self.inc_recurse() || self.file_list.is_empty() {
            return;
        }

        // Producer-selection seam. `build_file_list` has already decided whether
        // the lazy on-demand producer applies (single-source `-r`, no options
        // whose bytes need a global post-scan pass) and, if so, scanned only the
        // top level. In that case `file_list` holds exactly the initial segment;
        // seed the directory tree from it and defer every sub-list to the
        // transfer loop. Otherwise run the eager whole-list classify + reorder.
        if self.lazy_producer_active {
            debug_log!(Flist, 3, "inc_recurse producer: lazy (LF-2c)");
            self.seed_lazy_producer();
            debug_log!(
                Flist,
                2,
                "seeded lazy producer: {} initial entries",
                self.incremental.initial_segment_count.unwrap_or(0),
            );
            return;
        }
        debug_log!(Flist, 3, "inc_recurse producer: eager");

        let plan = protocol::flist::SubListPlan::new(self.file_list.as_slice());
        for &orphan in &plan.orphans {
            // upstream: flist.c:3228 - the receiver's wording for the same
            // condition. Reported here, on the side that can name the offending
            // source, and as FERROR_XFER so the run cannot exit 0 with a file
            // list the peer will reject.
            let text = format!(
                "rsync: [sender] parent directory is missing from the file list: {}\n",
                self.file_list[orphan].path().display()
            );
            self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
            self.add_io_error(io_error_flags::IOERR_GENERAL);
        }
        self.apply_sublist_plan(&plan);

        debug_log!(
            Flist,
            2,
            "partitioned file list: {} initial entries, {} sub-segments",
            self.incremental.initial_segment_count.unwrap_or(0),
            self.incremental.pending_segments.len()
        );
    }

    /// Reorders `file_list`/`source_bases` into the planned send order and
    /// builds the pending sub-list segments.
    ///
    /// Entries are moved, not cloned: the initial list first, then each
    /// sub-list's members in send order, so an entry's flat index is its
    /// position in the stream.
    fn apply_sublist_plan(&mut self, plan: &protocol::flist::SubListPlan) {
        let mut file_entries: Vec<Option<FileEntry>> = std::mem::take(&mut self.file_list)
            .into_vec()
            .into_iter()
            .map(Some)
            .collect();
        let mut bases: Vec<Option<Arc<Path>>> = std::mem::take(&mut self.source_bases)
            .into_iter()
            .map(Some)
            .collect();
        let total = file_entries.len();
        self.file_list = protocol::flist::DualFileList::with_capacity(total);
        self.source_bases = Vec::with_capacity(total);
        let mut flat_of = vec![usize::MAX; total];
        let mut place = |ctx: &mut Self, i: usize| -> usize {
            let flat = ctx.file_list.len();
            ctx.file_list.push(
                file_entries[i]
                    .take()
                    .expect("each entry is planned exactly once"),
            );
            ctx.source_bases.push(
                bases[i]
                    .take()
                    .expect("each source base is planned exactly once"),
            );
            flat
        };
        for &i in &plan.initial {
            flat_of[i] = place(self, i);
        }
        self.incremental.initial_segment_count = Some(plan.initial.len());
        self.incremental
            .ndx_map
            .set_initial_used(plan.initial.len());
        let mut pending = Vec::with_capacity(plan.sublists.len());
        for sublist in &plan.sublists {
            let flist_start = self.file_list.len();
            for &member in &sublist.members {
                flat_of[member] = place(self, member);
            }
            pending.push(PendingSegment {
                parent_dir_ndx: sublist.parent_dir_ndx,
                parent_flat_idx: flat_of[sublist.parent],
                flist_start,
                count: sublist.members.len(),
            });
        }
        self.incremental.pending_segments = pending;
        // upstream: flist.c:824-831 - hard-link leaders and gnums follow the
        // send order this reorder just fixed, gaps included. The lazy producer
        // never reaches here: `--hard-links` makes it ineligible.
        #[cfg(unix)]
        if self.config.flags.hard_links {
            self.assign_hardlink_indices();
        }
        // The initial list itemizes the transfer root `.` via its own gap NDX
        // (`ndx_start - 1`) when the first entry is `.`. upstream flist.c:2812
        // keeps `flist->parent_ndx` (pointing at dir_flist[0] == `.`) unless the
        // first sorted entry's basename differs from ".", in which case the root
        // has no directory row. `.` is always the first initial entry (flat 0)
        // when present, so resolve its slot to flat 0; otherwise leave it -1.
        self.incremental.ndx_map.set_initial_parent_flat(
            if self.file_list.get(0).is_some_and(|e| e.name() == ".") {
                0
            } else {
                -1
            },
        );
    }

    /// Scans one directory into a single pending segment - upstream's
    /// `send1extra()` / `send_extra_file_list()` per-directory body.
    ///
    /// Appends the directory's immediate children to `file_list`/`source_bases`
    /// via [`scan_one_directory`](Self::scan_one_directory) (one level; a
    /// sub-directory is recorded but not expanded), then sorts just that
    /// freshly-appended run by `f_name_cmp`, co-permuting `source_bases` in
    /// lockstep, so the segment's wire order equals the eager whole-list sort
    /// restricted to this directory - the byte-neutrality invariant the lazy
    /// producer must preserve. A single physical directory yields unique child
    /// names, so no dedup pass is needed here; duplicate-directory batching
    /// (FLAG_DUPLICATE) belongs to the producer loop.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send1extra()` - one directory, one sub-list
    /// - `flist.c:send_directory()` with `FLAG_DIVERT_DIRS` - one level, subdirs diverted
    pub(in crate::generator) fn scan_extra_segment(
        &mut self,
        base: &Path,
        dir_path: &Path,
        parent_dir_ndx: i32,
        parent_flat_idx: usize,
    ) -> std::io::Result<PendingSegment> {
        let flist_start = self.file_list.len();
        // upstream: flist.c:2229 interpret_stat_error - a directory that
        // vanished between the initial listing and its lazy scan is reported
        // as vanished (IOERR_VANISHED, exit 24), not as a general opendir
        // error, and yields an empty sub-list rather than aborting. Stat
        // before opendir so an ENOENT is attributed here, mirroring the
        // generator's own vanish idiom at protocol_io.rs record_open_failure.
        match std::fs::symlink_metadata(dir_path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.add_io_error(io_error_flags::IOERR_VANISHED);
                let fname = crate::full_fname::full_fname_path(dir_path, self.full_fname_paths());
                self.queue_flist_diagnostic(
                    SenderDiagnostic::Warning,
                    format!("file has vanished: {fname}\n"),
                );
                return Ok(PendingSegment {
                    parent_dir_ndx,
                    parent_flat_idx,
                    flist_start,
                    count: 0,
                });
            }
            Err(e) => return Err(e),
        }
        self.scan_one_directory(base, dir_path)?;
        self.file_list.sort_tail_with_parallel(
            flist_start,
            &mut self.source_bases,
            self.config.qsort,
        );
        let count = self.file_list.len() - flist_start;
        Ok(PendingSegment {
            parent_dir_ndx,
            parent_flat_idx,
            flist_start,
            count,
        })
    }

    /// Seeds the lazy producer from the already-built initial (top-level)
    /// segment - the lazy-producer counterpart of the eager reorder's phase 1.
    ///
    /// `file_list` at this point holds exactly the initial segment (the top
    /// level was scanned one level only). Each top-level directory becomes a
    /// tree node whose depth-first traversal drives on-demand sub-list
    /// scanning. Wire `dir_ndx` values are assigned identically to the eager
    /// path: every directory entry - including `.` and any duplicate - advances
    /// the counter (matching `reorder_and_build_segments` phase 1, which counts
    /// all dirs for `dir_flist` alignment), but only a real, non-duplicate
    /// directory owns a tree node and a sub-list.
    fn seed_lazy_producer(&mut self) {
        use protocol::flist::DirectoryTree;

        let mut tree = DirectoryTree::new();
        // Index 0 is the tree's virtual root; give the parallel Vecs a matching
        // placeholder so every node handle indexes them directly.
        let mut base_by_node: Vec<Arc<Path>> = vec![Arc::from(Path::new(""))];
        let mut flat_by_node: Vec<usize> = vec![usize::MAX];
        let mut wire_dir_ndx: i32 = 0;

        for i in 0..self.file_list.len() {
            let entry = &self.file_list[i];
            if !entry.is_dir() {
                continue;
            }
            // upstream: flist.c - the initial `dir_flist` counts every directory
            // (incl. `.` and FLAG_DUPLICATE siblings) so the receiver's dir_ndx
            // sequence stays aligned; only a real directory owns a sub-list.
            if entry.name() != "." && !entry.duplicate() {
                let name = entry.path().clone();
                let base = self.source_bases[i].clone();
                let handle = tree.add_directory(wire_dir_ndx as usize, name, None);
                debug_assert_eq!(handle, base_by_node.len(), "node handle tracks Vec length");
                base_by_node.push(base);
                flat_by_node.push(i);
            }
            wire_dir_ndx += 1;
        }

        // The whole current file_list IS the initial segment; sub-lists follow.
        self.incremental.initial_segment_count = Some(self.file_list.len());
        self.incremental
            .ndx_map
            .set_initial_used(self.file_list.len());
        // Same rule as the eager reorder: the transfer root `.` itemizes via its
        // own gap NDX only when it is the first sorted entry.
        self.incremental.ndx_map.set_initial_parent_flat(
            if self.file_list.get(0).is_some_and(|e| e.name() == ".") {
                0
            } else {
                -1
            },
        );

        self.lazy_producer = Some(LazyFlistProducer {
            tree,
            base_by_node,
            flat_by_node,
            next_wire_dir_ndx: wire_dir_ndx,
        });
    }

    /// Produces the next INC_RECURSE sub-list on demand - the lazy-producer counterpart
    /// of the eager reorder's phase 2, one directory per call.
    ///
    /// Pops the directory the depth-first cursor is on, scans it into one
    /// segment via [`scan_extra_segment`](Self::scan_extra_segment) (which sorts
    /// the freshly-appended run so its wire order equals the eager whole-list
    /// sort restricted to this directory), then links any child directories
    /// under the popped node - assigning each the next wire `dir_ndx` in the
    /// segment's sorted order - before advancing the cursor so the walk descends
    /// into them. Returns `None` once every directory has been scanned.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_extra_file_list()` / `send1extra()` - one directory, one
    ///   sub-list, children's subdirectories added to the tree afterwards
    /// - `flist.c:add_dirs_to_tree()` - depth-first child linking
    pub(in crate::generator) fn produce_next_lazy_segment(
        &mut self,
    ) -> std::io::Result<Option<PendingSegment>> {
        // Peek the current directory node (handle, wire dir_ndx, relative name)
        // and capture the scan base, dropping the tree borrow before scanning.
        let (handle, parent_dir_ndx, dir_rel, base) = {
            let Some(lp) = self.lazy_producer.as_ref() else {
                return Ok(None);
            };
            let Some((handle, dir_ndx, path)) = lp.tree.peek() else {
                return Ok(None);
            };
            (
                handle,
                dir_ndx as i32,
                path.to_path_buf(),
                lp.base_by_node[handle].clone(),
            )
        };
        let parent_flat_idx = self
            .lazy_producer
            .as_ref()
            .expect("producer present")
            .flat_by_node[handle];
        let dir_path = super::super::context::join_source_path(&base, &dir_rel);

        let segment = self.scan_extra_segment(&base, &dir_path, parent_dir_ndx, parent_flat_idx)?;

        // Collect the child directories in the freshly-sorted segment run, in
        // order, so wire dir_ndx assignment matches the eager phase-2 walk over
        // `seg.children`.
        let end = segment.flist_start + segment.count;
        let mut child_dirs: Vec<(usize, PathBuf)> = Vec::new();
        for i in segment.flist_start..end {
            let entry = &self.file_list[i];
            if entry.is_dir() {
                child_dirs.push((i, entry.path().clone()));
            }
        }

        let lp = self.lazy_producer.as_mut().expect("producer present");
        for (child_flat, child_name) in child_dirs {
            let child_wire = lp.next_wire_dir_ndx;
            lp.next_wire_dir_ndx += 1;
            let child_handle = lp
                .tree
                .add_directory(child_wire as usize, child_name, Some(handle));
            debug_assert_eq!(
                child_handle,
                lp.base_by_node.len(),
                "node handle tracks Vec length"
            );
            lp.base_by_node.push(base.clone());
            lp.flat_by_node.push(child_flat);
        }
        // Children are linked; now the depth-first cursor may descend into them.
        lp.tree.advance_cursor();

        Ok(Some(segment))
    }

    /// Whether the lazy producer has scanned every directory (no sub-lists
    /// remain to produce). Always `true` when the producer is inactive.
    pub(in crate::generator) fn lazy_scan_exhausted(&self) -> bool {
        self.lazy_producer
            .as_ref()
            .is_none_or(|lp| lp.tree.traversal_finished())
    }
}
