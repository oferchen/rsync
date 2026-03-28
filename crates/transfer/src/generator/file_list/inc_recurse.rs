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
//! exactly, or the receiver's dirname validation at `flist.c:2652-2659` will
//! reject entries with "ABORTING due to invalid path from sender".
//!
//! # Upstream Reference
//!
//! - `flist.c:add_dirs_to_tree()` - organizes dirs into traversal tree
//! - `flist.c:send_extra_file_list()` - sends one segment per directory
//! - `flist.c:recv_file_list()` line 2643 - appends dirs to `dir_flist`

use std::collections::HashMap;
use std::path::Path;

use logging::debug_log;
use protocol::flist::FileEntry;

use super::super::{DirSegment, GeneratorContext, PendingSegment, TaggedIndex};

impl GeneratorContext {
    /// Partitions the sorted file list into segments for incremental recursion.
    ///
    /// Reorders `file_list` and `full_paths` so that initial (top-level) entries
    /// come first, followed by sub-directory entries in depth-first order. This
    /// makes NDX values correspond directly to indices in the reordered list.
    pub(in crate::generator) fn partition_file_list_for_inc_recurse(&mut self) {
        if !self.inc_recurse() || self.file_list.is_empty() {
            return;
        }

        let classification = Self::classify_file_list_entries(&self.file_list);
        self.reorder_and_build_segments(classification);

        debug_log!(
            Flist,
            2,
            "partitioned file list: {} initial entries, {} sub-segments",
            self.incremental.initial_segment_count.unwrap_or(0),
            self.incremental.pending_segments.len()
        );
    }

    /// Classifies file list entries as top-level or nested directory children.
    ///
    /// Walks the file list and assigns each entry to either the initial (top-level)
    /// segment or to a per-directory segment. Directories are tagged with internal
    /// node IDs via [`TaggedIndex`] so the reorder phase can assign wire `dir_ndx`
    /// values without name-based HashMap lookups.
    fn classify_file_list_entries(file_list: &[FileEntry]) -> ClassificationResult {
        use protocol::flist::DirectoryTree;

        let mut tree = DirectoryTree::new();
        // Maps directory name to (tree_node_handle, segment_index, node_id).
        let mut dir_map: HashMap<String, (usize, usize, usize)> = HashMap::new();
        let mut initial_entries: Vec<TaggedIndex> = Vec::new();
        let mut segments: Vec<DirSegment> = Vec::new();
        let mut node_id_counter: usize = 0;

        for (i, entry) in file_list.iter().enumerate() {
            let name = entry.name();
            let parent = Path::new(name)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let is_top_level = parent.is_empty() || parent == ".";

            if is_top_level {
                let node_id = if entry.is_dir() && name != "." {
                    let id = node_id_counter;
                    node_id_counter += 1;
                    let tree_handle = tree.add_directory(id, name.to_string(), None);
                    let seg_idx = segments.len();
                    segments.push(DirSegment {
                        node_id: id,
                        children: Vec::new(),
                    });
                    dir_map.insert(name.to_string(), (tree_handle, seg_idx, id));
                    Some(id)
                } else {
                    None
                };
                initial_entries.push(TaggedIndex {
                    file_idx: i,
                    node_id,
                });
            } else if let Some(&(_, seg_idx, _)) = dir_map.get(parent.as_str()) {
                let node_id = if entry.is_dir() {
                    let id = node_id_counter;
                    node_id_counter += 1;
                    let parent_handle = dir_map.get(parent.as_str()).map(|&(h, _, _)| h);
                    let tree_handle = tree.add_directory(id, name.to_string(), parent_handle);
                    let new_seg_idx = segments.len();
                    segments.push(DirSegment {
                        node_id: id,
                        children: Vec::new(),
                    });
                    dir_map.insert(name.to_string(), (tree_handle, new_seg_idx, id));
                    Some(id)
                } else {
                    None
                };
                segments[seg_idx].children.push(TaggedIndex {
                    file_idx: i,
                    node_id,
                });
            } else {
                initial_entries.push(TaggedIndex {
                    file_idx: i,
                    node_id: None,
                });
            }
        }

        ClassificationResult {
            initial_entries,
            segments,
            tree,
            num_dirs: node_id_counter,
        }
    }

    /// Reorders file_list/full_paths and builds pending segments from classification.
    ///
    /// Computes wire `dir_ndx` values that match the upstream receiver's `dir_flist`
    /// growth order:
    /// 1. Initial list directories (including ".") get indices 0..N in sorted order
    /// 2. Sub-list directories get indices N.. in depth-first reception order
    ///
    /// Uses dense `Vec` indexed by node_id for O(1) wire `dir_ndx` lookups, and
    /// moves entries instead of cloning to avoid heap allocations.
    fn reorder_and_build_segments(&mut self, cr: ClassificationResult) {
        let ClassificationResult {
            initial_entries,
            segments,
            mut tree,
            num_dirs,
        } = cr;

        // Wrap in Option<T> for safe move-out by index.
        let mut file_entries: Vec<Option<FileEntry>> = std::mem::take(&mut self.file_list)
            .into_iter()
            .map(Some)
            .collect();
        let mut paths: Vec<Option<std::path::PathBuf>> = std::mem::take(&mut self.full_paths)
            .into_iter()
            .map(Some)
            .collect();

        let total = file_entries.len();
        self.file_list = Vec::with_capacity(total);
        self.full_paths = Vec::with_capacity(total);

        // Place initial entries first (move, not clone).
        for tagged in &initial_entries {
            self.file_list
                .push(file_entries[tagged.file_idx].take().unwrap());
            self.full_paths.push(paths[tagged.file_idx].take().unwrap());
        }
        self.incremental.initial_segment_count = Some(initial_entries.len());

        // Phase 1: Assign wire dir_ndx to initial list directories.
        // Dense Vec indexed by node_id - O(1) lookup.
        let mut node_to_wire: Vec<i32> = vec![-1; num_dirs];
        let mut wire_dir_ndx: i32 = 0;

        for (i, tagged) in initial_entries.iter().enumerate() {
            if self.file_list[i].is_dir() {
                if let Some(nid) = tagged.node_id {
                    node_to_wire[nid] = wire_dir_ndx;
                }
                // Count ALL dirs including "." for correct dir_flist alignment.
                wire_dir_ndx += 1;
            }
        }

        // Build dense node_id → segment index mapping.
        let mut node_to_seg: Vec<usize> = vec![usize::MAX; num_dirs];
        for (seg_idx, seg) in segments.iter().enumerate() {
            node_to_seg[seg.node_id] = seg_idx;
        }

        // Phase 2: Traverse tree depth-first, building PendingSegments with
        // correct wire dir_ndx values. Nested directories encountered in each
        // sub-list get the next wire_dir_ndx, matching the receiver's dir_flist
        // append order.
        let mut pending = Vec::new();
        while let Some((node_id, _path)) = tree.next_directory() {
            let seg_idx = node_to_seg[node_id];
            if seg_idx == usize::MAX {
                continue;
            }
            let seg = &segments[seg_idx];
            let flist_start = self.file_list.len();

            for child in &seg.children {
                self.file_list
                    .push(file_entries[child.file_idx].take().unwrap());
                self.full_paths.push(paths[child.file_idx].take().unwrap());

                if let Some(child_nid) = child.node_id {
                    node_to_wire[child_nid] = wire_dir_ndx;
                    wire_dir_ndx += 1;
                }
            }

            let parent_wire_ndx = node_to_wire[node_id];
            pending.push(PendingSegment {
                parent_dir_ndx: parent_wire_ndx,
                flist_start,
                count: seg.children.len(),
            });
        }

        self.incremental.pending_segments = pending;
    }
}

/// Intermediate result from file list classification, consumed by
/// [`GeneratorContext::reorder_and_build_segments`].
struct ClassificationResult {
    /// Tagged top-level entries with optional directory node IDs.
    initial_entries: Vec<TaggedIndex>,
    /// Per-directory segments with tagged child entries.
    segments: Vec<DirSegment>,
    /// Directory tree for depth-first traversal ordering.
    tree: protocol::flist::DirectoryTree,
    /// Total number of directories (for pre-allocating dense lookup Vecs).
    num_dirs: usize,
}
