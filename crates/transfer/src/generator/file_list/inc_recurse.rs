//! INC_RECURSE file list partitioning for the generator role.
//!
//! Partitions the sorted file list into per-directory segments for
//! incremental recursion sending. Top-level entries are placed first,
//! followed by sub-directory entries in depth-first order.
//!
//! # Upstream Reference
//!
//! - `flist.c:add_dirs_to_tree()` - organizes dirs into traversal tree
//! - `flist.c:send_extra_file_list()` - sends one segment per directory
//! - `flist.c:recv_file_list()` - `ndx_start = cur_flist->ndx_start + cur_flist->used`

use std::collections::HashMap;
use std::path::Path;

use logging::debug_log;
use protocol::flist::FileEntry;

use super::super::{GeneratorContext, PendingSegment, SegmentClassification};

impl GeneratorContext {
    /// Partitions the sorted file list into segments for incremental recursion.
    ///
    /// Reorders `file_list` and `full_paths` so that initial (top-level) entries
    /// come first, followed by sub-directory entries in depth-first order. This
    /// makes NDX values correspond directly to indices in the reordered list.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:add_dirs_to_tree()` - organizes dirs into traversal tree
    /// - `flist.c:send_extra_file_list()` - sends one segment per directory
    /// - `flist.c:recv_file_list()` - `ndx_start = cur_flist->ndx_start + cur_flist->used`
    pub(in crate::generator) fn partition_file_list_for_inc_recurse(&mut self) {
        if !self.inc_recurse() || self.file_list.is_empty() {
            return;
        }

        let (initial_indices, segment_data, tree) =
            Self::classify_file_list_entries(&self.file_list);

        self.reorder_and_build_segments(initial_indices, segment_data, tree);

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
    /// segment or to a per-directory segment. Directories are registered in a
    /// `DirectoryTree` for later depth-first traversal.
    ///
    /// Returns:
    /// - `initial_indices`: original file list indices for the top-level segment
    /// - `segment_data`: per-directory classification with child indices
    /// - `tree`: directory tree for depth-first traversal ordering
    fn classify_file_list_entries(
        file_list: &[FileEntry],
    ) -> (
        Vec<usize>,
        Vec<SegmentClassification>,
        protocol::flist::DirectoryTree,
    ) {
        use protocol::flist::DirectoryTree;

        let mut tree = DirectoryTree::new();
        let mut dir_map: HashMap<String, (usize, usize)> = HashMap::new();
        let mut initial_indices: Vec<usize> = Vec::new();
        let mut segment_data: Vec<SegmentClassification> = Vec::new();
        let mut dir_ndx_counter: usize = 0;

        for (i, entry) in file_list.iter().enumerate() {
            let name = entry.name();
            let parent = Path::new(name)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let is_top_level = parent.is_empty() || parent == ".";

            if is_top_level {
                initial_indices.push(i);
                if entry.is_dir() && name != "." {
                    let node = tree.add_directory(dir_ndx_counter, name.to_string(), None);
                    let seg_idx = segment_data.len();
                    segment_data.push(SegmentClassification {
                        dir_ndx: dir_ndx_counter,
                        child_indices: Vec::new(),
                    });
                    dir_map.insert(name.to_string(), (node, seg_idx));
                    dir_ndx_counter += 1;
                }
            } else if let Some(&(_, seg_idx)) = dir_map.get(parent.as_str()) {
                segment_data[seg_idx].child_indices.push(i);
                if entry.is_dir() {
                    let parent_node = dir_map.get(parent.as_str()).map(|&(n, _)| n);
                    let node = tree.add_directory(dir_ndx_counter, name.to_string(), parent_node);
                    let new_seg_idx = segment_data.len();
                    segment_data.push(SegmentClassification {
                        dir_ndx: dir_ndx_counter,
                        child_indices: Vec::new(),
                    });
                    dir_map.insert(name.to_string(), (node, new_seg_idx));
                    dir_ndx_counter += 1;
                }
            } else {
                initial_indices.push(i);
            }
        }

        (initial_indices, segment_data, tree)
    }

    /// Reorders file_list/full_paths and builds pending segments from classification results.
    ///
    /// The initial (top-level) entries are placed first, then sub-segments are appended
    /// in depth-first traversal order from the directory tree. This produces a contiguous
    /// layout where NDX = index into `self.file_list`.
    fn reorder_and_build_segments(
        &mut self,
        initial_indices: Vec<usize>,
        segment_data: Vec<SegmentClassification>,
        mut tree: protocol::flist::DirectoryTree,
    ) {
        let old_file_list = std::mem::take(&mut self.file_list);
        let old_full_paths = std::mem::take(&mut self.full_paths);
        self.file_list = Vec::with_capacity(old_file_list.len());
        self.full_paths = Vec::with_capacity(old_full_paths.len());

        for &idx in &initial_indices {
            self.file_list.push(old_file_list[idx].clone());
            self.full_paths.push(old_full_paths[idx].clone());
        }
        self.incremental.initial_segment_count = Some(initial_indices.len());

        // Build dir_ndx -> segment_data index mapping for O(1) lookup
        let dir_ndx_to_seg: HashMap<usize, usize> = segment_data
            .iter()
            .enumerate()
            .map(|(seg_idx, seg)| (seg.dir_ndx, seg_idx))
            .collect();

        let mut pending = Vec::new();
        while let Some((dir_ndx, _path)) = tree.next_directory() {
            if let Some(&seg_idx) = dir_ndx_to_seg.get(&dir_ndx) {
                let seg = &segment_data[seg_idx];
                let flist_start = self.file_list.len();
                for &idx in &seg.child_indices {
                    self.file_list.push(old_file_list[idx].clone());
                    self.full_paths.push(old_full_paths[idx].clone());
                }
                pending.push(PendingSegment {
                    parent_dir_ndx: seg.dir_ndx as i32,
                    flist_start,
                    count: seg.child_indices.len(),
                });
            }
        }

        self.incremental.pending_segments = pending;
    }
}
