//! File list building, walking, and sorting for the generator role.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use logging::{debug_log, info_log};
use protocol::flist::{FileEntry, compare_file_entries};

#[cfg(unix)]
use protocol::flist::{DevIno, HardlinkLookup, HardlinkTable};

use super::io_error_flags;
use super::{GeneratorContext, PendingSegment, SegmentClassification};

impl GeneratorContext {
    /// Assigns hardlink indices to entries sharing the same (dev, ino) pair.
    ///
    /// Must be called after sorting since indices are post-sort file list positions.
    /// The first occurrence in sorted order becomes the leader (`u32::MAX`); subsequent
    /// occurrences become followers pointing to the leader's index.
    ///
    /// Entries with `hardlink_dev`/`hardlink_ino` set during `create_entry()` are
    /// matched here. After assignment, the temporary dev/ino fields are cleared for
    /// protocol >= 30 (which uses index-based hardlink encoding on the wire).
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:match_hard_links()` — called after `sort_file_list()`
    /// - `hlink.c:idev_find()` — two-level (dev, ino) hashtable lookup
    #[cfg(unix)]
    fn assign_hardlink_indices(&mut self) {
        let mut table = HardlinkTable::new();

        for i in 0..self.file_list.len() {
            let entry = &self.file_list[i];
            let (Some(dev), Some(ino)) = (entry.hardlink_dev(), entry.hardlink_ino()) else {
                continue;
            };

            let dev_ino = DevIno::new(dev as u64, ino as u64);
            match table.find_or_insert(dev_ino, i as u32) {
                HardlinkLookup::First(_) => {
                    // Leader: mark with u32::MAX (XMIT_HLINK_FIRST on wire)
                    self.file_list[i].set_hardlink_idx(u32::MAX);
                }
                HardlinkLookup::LinkTo(leader_ndx) => {
                    // Follower: point to leader's sorted index
                    self.file_list[i].set_hardlink_idx(leader_ndx);
                }
            }

            // Clear temporary dev/ino for proto 30+ (not sent on wire)
            if self.protocol.as_u8() >= 30 {
                self.file_list[i].set_hardlink_dev(0);
                self.file_list[i].set_hardlink_ino(0);
            }
        }
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
        use metadata::id_lookup::{lookup_group_name, lookup_user_name};

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
        self.timing.flist_build_start = Some(Instant::now());

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

        // Assign hardlink indices after sort (indices are post-sort file list positions).
        // upstream: hlink.c:match_hard_links() called after sort_file_list()
        #[cfg(unix)]
        if self.config.flags.hard_links {
            self.assign_hardlink_indices();
        }

        // Record end time for flist_buildtime statistic
        self.timing.flist_build_end = Some(Instant::now());

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

    /// Builds a file list from `--files-from` entries using a shared base directory.
    ///
    /// Unlike [`build_file_list`](Self::build_file_list), which treats each path as
    /// its own base for `walk_path`, this method uses a single `base_dir` for all
    /// file paths. Each entry's relative name is computed by stripping `base_dir`,
    /// matching upstream rsync's behaviour of `chdir(argv[0])` before reading
    /// filenames from the `--files-from` source.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2240-2264` - `change_dir(argv[0])` then read relative filenames
    /// - `flist.c:2262` - `read_line(filesfrom_fd, ...)` reads one name at a time
    pub fn build_file_list_with_base(
        &mut self,
        base_dir: &Path,
        file_paths: &[PathBuf],
    ) -> io::Result<usize> {
        self.timing.flist_build_start = Some(Instant::now());

        info_log!(Flist, 1, "building file list from --files-from...");
        self.clear_file_list();

        const FLIST_START: usize = 4096;
        self.file_list.reserve(FLIST_START);
        self.full_paths.reserve(FLIST_START);

        // upstream: flist.c:2287 — emit "." with XMIT_TOP_DIR for the root
        // transfer directory so --delete works correctly on the receiver side.
        if let Ok(meta) = std::fs::symlink_metadata(base_dir) {
            if meta.is_dir() {
                let mut dot_entry = self.create_entry(base_dir, PathBuf::from("."), &meta)?;
                dot_entry.set_flags(protocol::flist::FileFlags::new(
                    protocol::flist::XMIT_TOP_DIR,
                    0,
                ));
                self.push_file_item(dot_entry, base_dir.to_path_buf());
            }
        }

        // Walk each listed file using the shared base directory so that
        // relative paths are computed correctly (e.g., "hello.txt" instead
        // of an empty string).
        for path in file_paths {
            self.walk_path(base_dir, path.clone())?;
        }

        // Sort file list using rsync's ordering (upstream flist.c:f_name_cmp).
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

        apply_permutation_in_place(&mut self.file_list, &mut self.full_paths, indices);

        #[cfg(unix)]
        if self.config.flags.hard_links {
            self.assign_hardlink_indices();
        }

        self.timing.flist_build_end = Some(Instant::now());
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
        // upstream: flist.c:readlink_stat() — resolve symlinks based on flags:
        // --copy-links: follow ALL symlinks (stat instead of lstat)
        // --copy-unsafe-links: follow only UNSAFE symlinks (stat if target escapes tree)
        // otherwise: use lstat (preserve symlinks as-is)
        let metadata = match self.resolve_symlink_metadata(&path, base) {
            Ok(m) => m,
            Err(e) => {
                // upstream: flist.c:1286-1294 — log vanished warning or general error
                if e.kind() == io::ErrorKind::NotFound {
                    eprintln!(
                        "file has vanished: {}{}",
                        path.display(),
                        crate::role_trailer::sender()
                    );
                }
                self.record_io_error(&e);
                return Ok(());
            }
        };

        // Calculate relative path
        let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();

        // upstream: flist.c:2287 — always emit "." with XMIT_TOP_DIR for the
        // root transfer directory. Enables delete_in_dir() when --delete is active.
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            let mut dot_entry = self.create_entry(&path, PathBuf::from("."), &metadata)?;
            dot_entry.set_flags(protocol::flist::FileFlags::new(
                protocol::flist::XMIT_TOP_DIR,
                0,
            ));
            self.push_file_item(dot_entry, path.clone());

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

        // upstream: generator.c:1547 — skip unsafe symlinks when --safe-links.
        // Sender-side filtering ensures unsafe symlinks never reach the receiver,
        // matching the belt-and-suspenders approach for daemon push interop.
        if self.config.flags.safe_links && metadata.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                if super::super::symlink_safety::is_unsafe_symlink(target.as_os_str(), &relative) {
                    return Ok(());
                }
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
    /// Resolves symlink metadata following upstream `flist.c:readlink_stat()`.
    ///
    /// Three modes of symlink resolution:
    /// - `--copy-links`: follow ALL symlinks (stat instead of lstat)
    /// - `--copy-unsafe-links`: follow only symlinks whose target escapes
    ///   the transfer tree (converting them to regular files)
    /// - Default: use lstat (preserve symlinks as symlinks)
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:205-232` - `readlink_stat()`
    /// - `flist.c:215` - `copy_unsafe_links && unsafe_symlink(linkbuf, path)`
    fn resolve_symlink_metadata(&self, path: &Path, base: &Path) -> io::Result<std::fs::Metadata> {
        if self.config.flags.copy_links {
            return std::fs::metadata(path);
        }

        let meta = std::fs::symlink_metadata(path)?;

        // upstream: flist.c:215 - follow unsafe symlinks when --copy-unsafe-links
        if self.config.flags.copy_unsafe_links && meta.file_type().is_symlink() {
            let target = std::fs::read_link(path)?;
            let relative = path.strip_prefix(base).unwrap_or(path);
            if super::super::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative) {
                return std::fs::metadata(path);
            }
        }

        Ok(meta)
    }

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

        // Set access time if preserving (upstream: flist.c:489-494)
        #[cfg(unix)]
        if self.config.flags.atimes && !entry.is_dir() {
            entry.set_atime(metadata.atime());
        }
        #[cfg(not(unix))]
        if self.config.flags.atimes && !entry.is_dir() {
            if let Ok(atime) = metadata.accessed() {
                if let Ok(duration) = atime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_atime(duration.as_secs() as i64);
                }
            }
        }

        // Set creation time if preserving (upstream: flist.c:495-498)
        if self.config.flags.crtimes {
            if let Ok(crtime) = metadata.created() {
                if let Ok(duration) = crtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_crtime(duration.as_secs() as i64);
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

        // Store dev/ino for hardlink detection (post-sort assignment).
        // upstream: flist.c:make_file() stores tmp_dev/tmp_ino when preserve_hard_links
        #[cfg(unix)]
        if self.config.flags.hard_links && metadata.nlink() > 1 && !metadata.is_dir() {
            entry.set_hardlink_dev(metadata.dev() as i64);
            entry.set_hardlink_ino(metadata.ino() as i64);
        }

        Ok(entry)
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
