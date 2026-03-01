//! File list building and transmission for the generator role.
//!
//! This submodule handles:
//! - Filesystem walking and file entry creation
//! - File list sorting and partitioning for incremental recursion
//! - File list wire transmission (initial + extra sub-lists)
//! - UID/GID collection for name-based ownership transfer
//! - Filter rule parsing from wire format
//!
//! # Upstream Reference
//!
//! - `flist.c` — File list building and transmission
//! - `flist.c:send_file_list()` — Main file list builder
//! - `flist.c:send_extra_file_list()` — Incremental recursion sub-lists

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use filters::{FilterRule, FilterSet};
use logging::{debug_log, info_log};
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::filters::{FilterRuleWireFormat, RuleType};
use protocol::flist::{FileEntry, FileListWriter, compare_file_entries};

#[cfg(unix)]
use metadata::id_lookup::{lookup_group_name, lookup_user_name};

use protocol::CompatibilityFlags;

use super::GeneratorContext;
use super::io_error_flags;

/// A pending file list sub-segment for incremental recursion sending.
///
/// References entries in `GeneratorContext::file_list` by range rather than
/// storing cloned entries, avoiding double allocation.
#[derive(Debug)]
pub(crate) struct PendingSegment {
    /// Global NDX of the parent directory.
    pub(super) parent_dir_ndx: i32,
    /// Start index into `GeneratorContext::file_list`.
    pub(super) flist_start: usize,
    /// Number of entries in this segment.
    pub(super) count: usize,
}

/// Classification of a directory's children for incremental recursion.
///
/// Groups the original file list indices belonging to a single directory
/// segment, along with the directory's NDX used as parent reference when
/// sending the segment over the wire.
#[derive(Debug)]
struct SegmentClassification {
    /// Directory NDX assigned to this directory.
    dir_ndx: usize,
    /// Original file list indices of entries belonging to this directory.
    child_indices: Vec<usize>,
}

impl GeneratorContext {
    /// Creates a configured `FileListWriter` matching the current protocol and flags.
    pub(super) fn build_flist_writer(&self) -> FileListWriter {
        let mut writer = if let Some(flags) = self.compat_flags {
            FileListWriter::with_compat_flags(self.protocol, flags)
        } else {
            FileListWriter::new(self.protocol)
        }
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group)
        .with_preserve_links(self.config.flags.links)
        .with_preserve_devices(self.config.flags.devices)
        .with_preserve_specials(self.config.flags.specials)
        .with_preserve_hard_links(self.config.flags.hard_links)
        .with_preserve_atimes(self.config.flags.atimes)
        .with_preserve_acls(self.config.flags.acls)
        .with_preserve_xattrs(self.config.flags.xattrs);

        if let Some(ref converter) = self.config.iconv {
            writer = writer.with_iconv(converter.clone());
        }
        writer
    }

    /// Adds a file entry and its corresponding full path to the file list.
    ///
    /// This method maintains the invariant that `file_list` and `full_paths`
    /// have the same length and corresponding entries at each index.
    pub(super) fn push_file_item(&mut self, entry: FileEntry, full_path: PathBuf) {
        debug_assert_eq!(
            self.file_list.len(),
            self.full_paths.len(),
            "file_list and full_paths must be kept in sync before push"
        );
        self.file_list.push(entry);
        self.full_paths.push(full_path);
    }

    /// Clears both the file list and full paths arrays.
    ///
    /// This method maintains the invariant that both arrays are cleared together.
    pub(super) fn clear_file_list(&mut self) {
        self.file_list.clear();
        self.full_paths.clear();
    }

    /// Collects unique UID/GID values from the file list and looks up their names.
    ///
    /// This must be called after `build_file_list` and before `send_id_lists`.
    /// On non-Unix platforms, this is a no-op since ownership is not preserved.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:add_uid()` / `add_gid()` - called during file list building
    #[cfg(unix)]
    pub fn collect_id_mappings(&mut self) {
        // Skip if numeric_ids is set - no name mapping needed
        if self.config.flags.numeric_ids {
            return;
        }

        self.uid_list.clear();
        self.gid_list.clear();

        for entry in &self.file_list {
            // Collect UIDs if preserving ownership
            if self.config.flags.owner {
                if let Some(uid) = entry.uid() {
                    // Skip expensive lookup if we already have this UID
                    if !self.uid_list.contains(uid) {
                        let name = lookup_user_name(uid).ok().flatten();
                        self.uid_list.add_id(uid, name);
                    }
                }
            }

            // Collect GIDs if preserving group
            if self.config.flags.group {
                if let Some(gid) = entry.gid() {
                    // Skip expensive lookup if we already have this GID
                    if !self.gid_list.contains(gid) {
                        let name = lookup_group_name(gid).ok().flatten();
                        self.gid_list.add_id(gid, name);
                    }
                }
            }
        }
    }

    /// Collects unique UID/GID values from the file list.
    /// No-op on non-Unix platforms since ownership is not preserved.
    #[cfg(not(unix))]
    pub fn collect_id_mappings(&mut self) {
        // No-op on non-Unix platforms
    }

    /// Sends NDX_FLIST_EOF if incremental recursion is enabled.
    ///
    /// This signals to the receiver that there are no more incremental file lists.
    /// For a simple (non-recursive directory) transfer, `send_dir_ndx` is -1, so we
    /// always send `NDX_FLIST_EOF` when INC_RECURSE is enabled.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2534-2545` in `send_file_list()`:
    ///   ```c
    ///   if (inc_recurse) {
    ///       if (send_dir_ndx < 0) {
    ///           write_ndx(f, NDX_FLIST_EOF);
    ///           flist_eof = 1;
    ///       }
    ///   }
    ///   ```
    pub(super) fn send_flist_eof_if_inc_recurse<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> io::Result<()> {
        if self.flist_eof_sent {
            return Ok(());
        }
        if let Some(flags) = self.compat_flags
            && flags.contains(CompatibilityFlags::INC_RECURSE)
        {
            let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());
            ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
            writer.flush()?;
            self.flist_eof_sent = true;
        }
        Ok(())
    }

    /// Builds the file list from the specified paths.
    ///
    /// This walks the filesystem starting from each path in the arguments
    /// and builds a sorted file list for transmission.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2192` - `send_file_list()` - Main file list builder
    /// - `flist.c:1456` - `send_file_entry()` - Per-file encoding
    ///
    /// Mirrors upstream recursive directory scanning and file list construction behavior.
    pub fn build_file_list(&mut self, base_paths: &[PathBuf]) -> io::Result<usize> {
        // Track timing for flist_buildtime statistic (upstream stats.flist_buildtime)
        self.flist_build_start = Some(Instant::now());

        info_log!(Flist, 1, "building file list...");
        self.clear_file_list();

        // Pre-allocate capacity to reduce reallocations during recursive walk.
        // Upstream rsync pre-allocates FLIST_START_LARGE = 32768 pointer slots
        // (flist.c:2192). clear() retains existing capacity, so this only
        // allocates on the first call.
        const FLIST_START: usize = 4096;
        self.file_list.reserve(FLIST_START);
        self.full_paths.reserve(FLIST_START);

        for base_path in base_paths {
            self.walk_path(base_path, base_path.clone())?;
        }

        // Sort file list using rsync's ordering (upstream flist.c:f_name_cmp).
        // We need to sort both file_list and full_paths together to maintain correspondence.
        // Create index array, sort by rsync rules, then reorder both arrays.
        // When --qsort is set, use unstable sort (upstream: flist.c:2991).
        let file_list_ref = &self.file_list;
        let mut indices: Vec<usize> = {
            let len = self.file_list.len();
            let mut v = Vec::with_capacity(len);
            v.extend(0..len);
            v
        };
        let cmp =
            |&a: &usize, &b: &usize| compare_file_entries(&file_list_ref[a], &file_list_ref[b]);
        if self.config.qsort {
            indices.sort_unstable_by(cmp);
        } else {
            indices.sort_by(cmp);
        }

        // Apply permutation in-place using cycle-following algorithm.
        // This avoids cloning every element - O(n) swaps instead of O(n) clones.
        apply_permutation_in_place(&mut self.file_list, &mut self.full_paths, indices);

        // Record end time for flist_buildtime statistic
        self.flist_build_end = Some(Instant::now());

        // Collect UID/GID mappings for name-based ownership transfer
        self.collect_id_mappings();

        let count = self.file_list.len();
        info_log!(Flist, 1, "built file list with {} entries", count);
        debug_log!(Flist, 2, "file list entries: {:?}", {
            let mut names = Vec::with_capacity(count);
            names.extend(self.file_list.iter().map(FileEntry::name));
            names
        });

        Ok(count)
    }

    /// Partitions the sorted file list into segments for incremental recursion.
    ///
    /// Reorders `file_list` and `full_paths` so that initial (top-level) entries
    /// come first, followed by sub-directory entries in depth-first order. This
    /// makes NDX values correspond directly to indices in the reordered list.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:add_dirs_to_tree()` — organizes dirs into traversal tree
    /// - `flist.c:send_extra_file_list()` — sends one segment per directory
    /// - `flist.c:recv_file_list()` — `ndx_start = cur_flist->ndx_start + cur_flist->used`
    pub(super) fn partition_file_list_for_inc_recurse(&mut self) {
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
            self.initial_segment_count.unwrap_or(0),
            self.pending_segments.len()
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
        self.initial_segment_count = Some(initial_indices.len());

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

        self.pending_segments = pending;
    }

    /// Sends pending file list segments during the transfer loop.
    ///
    /// Called before reading each NDX request from the receiver. Since we eagerly
    /// scanned all files, this sends all pending segments at once on the first call.
    /// Subsequent calls are no-ops.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:send_files()` line 250: `send_extra_file_list(f, MIN_FILECNT_LOOKAHEAD)`
    /// - `flist.c:send_extra_file_list()` — sends one directory's entries
    pub(super) fn send_extra_file_lists<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if !self.inc_recurse() || self.flist_eof_sent || self.pending_segments.is_empty() {
            return Ok(());
        }

        // Reuse the cached writer from send_file_list() to preserve compression
        // state across sub-lists, matching upstream's static variables in
        // send_file_entry() (prev_name, prev_mode, prev_uid, prev_gid).
        let mut flist_writer = self
            .flist_writer_cache
            .take()
            .unwrap_or_else(|| self.build_flist_writer());

        // Send all pending segments. Since we eagerly scanned, send them all at once
        // (upstream uses MIN_FILECNT_LOOKAHEAD=1000 for throttling with lazy scanning).
        let segments = std::mem::take(&mut self.pending_segments);
        let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());

        for segment in &segments {
            if segment.count == 0 {
                continue;
            }

            // Build ndx_segments entry for this sub-list.
            // upstream: flist.c:2931 — flist->ndx_start = prev->ndx_start + prev->used + 1
            let &(prev_flat_start, prev_ndx_start) =
                self.ndx_segments.last().expect("initial segment exists");
            let prev_used = (segment.flist_start - prev_flat_start) as i32;
            let seg_ndx_start = prev_ndx_start + prev_used + 1;
            self.ndx_segments.push((segment.flist_start, seg_ndx_start));

            // Write NDX_FLIST_OFFSET - dir_ndx to signal a new sub-list
            ndx_codec.write_ndx(writer, NDX_FLIST_OFFSET - segment.parent_dir_ndx)?;

            // Write file entries from the reordered file_list
            let end = segment.flist_start + segment.count;
            for entry in &self.file_list[segment.flist_start..end] {
                flist_writer.write_entry(writer, entry)?;
            }

            // Write end-of-flist marker (zero byte)
            flist_writer.write_end(writer, None)?;

            debug_log!(
                Flist,
                2,
                "sent sub-list for dir_ndx={}, {} entries (ndx_start={})",
                segment.parent_dir_ndx,
                segment.count,
                seg_ndx_start
            );
        }

        // All segments sent — send NDX_FLIST_EOF
        ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
        writer.flush()?;
        self.flist_eof_sent = true;
        debug_log!(
            Flist,
            2,
            "sent NDX_FLIST_EOF, all {} sub-lists dispatched",
            segments.len()
        );

        Ok(())
    }

    /// Recursively walks a path and adds entries to the file list.
    ///
    /// # Upstream Reference
    ///
    /// When the source path is a directory ending with '/', upstream rsync includes
    /// the directory itself as "." entry in the file list. This allows the receiver
    /// to create the destination directory and properly set its attributes.
    ///
    /// See flist.c:send_file_list() which adds "." for the top-level directory.
    fn walk_path(&mut self, base: &Path, path: PathBuf) -> io::Result<()> {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                // Record error and continue (upstream rsync behavior)
                self.record_io_error(&e);
                return Ok(());
            }
        };

        // Calculate relative path
        let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();

        // For the base directory, skip the "." entry and just walk children
        // Some clients may not expect/handle the "." entry correctly
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            // Walk children of the base directory (no "." entry)
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => {
                                self.walk_path(base, entry.path())?;
                            }
                            Err(e) => {
                                // Entry vanished or unreadable during iteration
                                self.record_io_error(&e);
                            }
                        }
                    }
                }
                Err(e) => {
                    self.record_io_error(&e);
                }
            }
            return Ok(());
        }

        // Skip file types that are not being preserved.
        // upstream: flist.c:send_file_name() / make_file() skips unsupported types.
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            let ft = metadata.file_type();
            if (ft.is_block_device() || ft.is_char_device()) && !self.config.flags.devices {
                return Ok(());
            }
            if (ft.is_fifo() || ft.is_socket()) && !self.config.flags.specials {
                return Ok(());
            }
        }

        // Check filters if present
        if let Some(ref filters) = self.filters {
            let is_dir = metadata.is_dir();
            if !filters.allows(&relative, is_dir) {
                // Path is excluded by filters, skip it
                return Ok(());
            }
        }

        // Create file entry based on type (moves relative — no clone)
        let entry = match self.create_entry(&path, relative, &metadata) {
            Ok(e) => e,
            Err(_) => {
                // Failed to create entry (e.g., symlink target unreadable)
                self.add_io_error(io_error_flags::IOERR_GENERAL);
                return Ok(());
            }
        };

        // Read directory entries BEFORE moving path into push_file_item.
        // Upstream rsync: flist.c send_file_list() similarly scans then records.
        let should_recurse = metadata.is_dir() && self.config.flags.recursive;
        let dir_entries = if should_recurse {
            match std::fs::read_dir(&path) {
                Ok(entries) => Some(entries),
                Err(e) => {
                    self.record_io_error(&e);
                    None
                }
            }
        } else {
            None
        };

        self.push_file_item(entry, path); // Move, no clone

        // Recurse into directories if recursive mode is enabled
        if let Some(entries) = dir_entries {
            for dir_entry in entries {
                match dir_entry {
                    Ok(de) => {
                        self.walk_path(base, de.path())?;
                    }
                    Err(e) => {
                        self.record_io_error(&e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Creates a file entry from path and metadata.
    ///
    /// The `full_path` is used for filesystem operations (e.g., reading symlink targets),
    /// while `relative_path` is stored in the entry for transmission to the receiver.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:make_file()` — determines file type and populates the `file_struct`.
    /// - Device files (block/char) use `new_block_device`/`new_char_device` with rdev fields.
    /// - Special files (FIFOs/sockets) use `new_fifo`/`new_socket`.
    fn create_entry(
        &self,
        full_path: &Path,
        relative_path: PathBuf,
        metadata: &std::fs::Metadata,
    ) -> io::Result<FileEntry> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let file_type = metadata.file_type();

        let mut entry = if file_type.is_file() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };

            FileEntry::new_file(relative_path, metadata.len(), mode)
        } else if file_type.is_dir() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = 0o755;

            FileEntry::new_directory(relative_path, mode)
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(full_path).unwrap_or_else(|_| PathBuf::from(""));

            FileEntry::new_symlink(relative_path, target)
        } else {
            // Device and special file types (Unix only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                let mode = metadata.mode() & 0o7777;
                if file_type.is_block_device() {
                    let (major, minor) = rdev_to_major_minor(metadata.rdev());
                    FileEntry::new_block_device(relative_path, mode, major, minor)
                } else if file_type.is_char_device() {
                    let (major, minor) = rdev_to_major_minor(metadata.rdev());
                    FileEntry::new_char_device(relative_path, mode, major, minor)
                } else if file_type.is_fifo() {
                    FileEntry::new_fifo(relative_path, mode)
                } else if file_type.is_socket() {
                    FileEntry::new_socket(relative_path, mode)
                } else {
                    FileEntry::new_file(relative_path, 0, 0o644)
                }
            }
            #[cfg(not(unix))]
            {
                FileEntry::new_file(relative_path, 0, 0o644)
            }
        };

        // Set modification time
        #[cfg(unix)]
        {
            entry.set_mtime(metadata.mtime(), metadata.mtime_nsec() as u32);
        }
        #[cfg(not(unix))]
        {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_mtime(duration.as_secs() as i64, duration.subsec_nanos());
                }
            }
        }

        // Set ownership if preserving
        #[cfg(unix)]
        if self.config.flags.owner {
            entry.set_uid(metadata.uid());
        }
        #[cfg(unix)]
        if self.config.flags.group {
            entry.set_gid(metadata.gid());
        }

        Ok(entry)
    }

    /// Sends the file list to the receiver.
    pub fn send_file_list<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<usize> {
        // Track timing for flist_xfertime statistic (upstream stats.flist_xfertime)
        self.flist_xfer_start = Some(Instant::now());

        let mut flist_writer = self.build_flist_writer();

        // When INC_RECURSE, only send initial segment entries; the rest
        // are sent via send_extra_file_lists() during the transfer loop.
        let entries_to_send = if let Some(count) = self.initial_segment_count {
            &self.file_list[..count]
        } else {
            &self.file_list
        };
        for entry in entries_to_send {
            flist_writer.write_entry(writer, entry)?;
        }

        // Write end marker with io_error if any (SAFE_FILE_LIST support)
        let io_error_for_end = if self.io_error != 0 {
            Some(self.io_error)
        } else {
            None
        };
        flist_writer.write_end(writer, io_error_for_end)?;
        writer.flush()?;

        // Cache the writer so send_extra_file_lists() inherits the compression
        // state (prev_name, prev_mode, etc.), matching upstream's static variables.
        self.flist_writer_cache = Some(flist_writer);

        // Record end time for flist_xfertime statistic
        self.flist_xfer_end = Some(Instant::now());

        Ok(self.file_list.len())
    }

    /// Converts wire format rules to FilterSet.
    ///
    /// Maps the wire protocol representation to the filters crate's `FilterSet`
    /// for use during file walking.
    pub(super) fn parse_received_filters(
        &self,
        wire_rules: &[FilterRuleWireFormat],
    ) -> io::Result<FilterSet> {
        let mut rules = Vec::with_capacity(wire_rules.len());

        for wire_rule in wire_rules {
            // Convert wire RuleType to FilterRule
            let mut rule = match wire_rule.rule_type {
                RuleType::Include => FilterRule::include(&wire_rule.pattern),
                RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
                RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
                RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
                RuleType::Clear => {
                    // Clear rule removes all previous rules
                    rules.push(
                        FilterRule::clear()
                            .with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                    );
                    continue;
                }
                RuleType::Merge | RuleType::DirMerge => {
                    // Merge rules require per-directory filter file loading during file walking.
                    // Implementation requires:
                    // 1. Store merge rule specs (filename, options like inherit/exclude_self)
                    // 2. During build_file_list(), check each directory for the merge file
                    // 3. Parse merge file contents using engine::local_copy::dir_merge parsing
                    // 4. Inject parsed rules into the active FilterSet for that subtree
                    // 5. Pop rules when leaving directories (if no_inherit is set)
                    //
                    // See crates/engine/src/local_copy/dir_merge/ for the local copy implementation
                    // that can be adapted for server mode. The challenge is that FilterSet is
                    // currently immutable after construction.
                    //
                    // For now, clients can pre-expand merge rules before transmission, or use
                    // local copy mode which fully supports merge rules.
                    continue;
                }
            };

            // Apply modifiers
            if wire_rule.sender_side || wire_rule.receiver_side {
                rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
            }

            if wire_rule.perishable {
                rule = rule.with_perishable(true);
            }

            if wire_rule.xattr_only {
                rule = rule.with_xattr_only(true);
            }

            if wire_rule.negate {
                rule = rule.with_negate(true);
            }

            if wire_rule.anchored {
                rule = rule.anchor_to_root();
            }

            // Note: directory_only, no_inherit, cvs_exclude, word_split, exclude_from_merge
            // are pattern modifiers handled by the filters crate during compilation
            // We store them in the pattern itself as upstream does

            rules.push(rule);
        }

        FilterSet::from_rules(rules)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("filter error: {e}")))
    }
}

/// Applies a source-based permutation to two slices in-place using cycle-following.
///
/// This reorders elements according to the permutation `source_indices` without
/// cloning elements - only swaps are used. The algorithm inverts the permutation
/// and then follows each cycle, placing elements in their final positions.
///
/// # Arguments
/// * `slice_a` - First slice to reorder
/// * `slice_b` - Second slice to reorder (must have same length)
/// * `source_indices` - Source-based permutation where `source_indices[i]` is the
///   index of the element that should end up at position `i`
///
/// # Time Complexity
/// O(n) time and O(n) space for the inverse permutation.
pub(super) fn apply_permutation_in_place<A, B>(
    slice_a: &mut [A],
    slice_b: &mut [B],
    source_indices: Vec<usize>,
) {
    let n = slice_a.len();
    debug_assert_eq!(slice_b.len(), n);
    debug_assert_eq!(source_indices.len(), n);

    if n == 0 {
        return;
    }

    // Invert the permutation: source_indices[i] = j becomes dest_perm[j] = i
    // This converts "element at j goes to i" to "element at i goes to j"
    let mut dest_perm = vec![0; n];
    for (new_pos, &old_pos) in source_indices.iter().enumerate() {
        dest_perm[old_pos] = new_pos;
    }

    // Apply destination-based permutation using cycle-following
    for i in 0..n {
        while dest_perm[i] != i {
            let j = dest_perm[i];
            slice_a.swap(i, j);
            slice_b.swap(i, j);
            dest_perm.swap(i, j);
        }
    }
}

/// Extracts major and minor device numbers from a raw `rdev` value.
///
/// The layout differs by platform:
/// - **Linux**: Split encoding where major/minor span non-contiguous bits.
/// - **macOS/BSD**: Major in high byte, minor in low 24 bits.
///
/// # Upstream Reference
///
/// Mirrors glibc `major()`/`minor()` macros used by upstream rsync to populate
/// `rdev_major`/`rdev_minor` in `file_struct`.
#[cfg(all(unix, target_os = "linux"))]
pub(super) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32);
    let minor = (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32);
    (major, minor)
}

/// Extracts major and minor device numbers from a raw `rdev` value (BSD/macOS).
#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = (rdev >> 24) as u32;
    let minor = (rdev & 0xffffff) as u32;
    (major, minor)
}
