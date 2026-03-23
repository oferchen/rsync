//! File list building, walking, and sorting for the generator role.
//!
//! Implements filesystem scanning (`walk_path`), `FileEntry` construction
//! (`create_entry`), hardlink index assignment, UID/GID collection, and
//! INC_RECURSE partitioning into per-directory segments.
//!
//! # Submodules
//!
//! - `walk` - Recursive directory traversal and symlink resolution
//! - `entry` - `FileEntry` construction from filesystem metadata
//! - `hardlinks` - Hardlink index assignment and UID/GID collection
//! - `inc_recurse` - INC_RECURSE file list partitioning
//!
//! # Upstream Reference
//!
//! - `flist.c:2192` - `send_file_list()` main file list builder
//! - `flist.c:1456` - `send_file_entry()` per-file encoding
//! - `hlink.c:match_hard_links()` - post-sort hardlink index assignment

mod entry;
mod hardlinks;
mod inc_recurse;
mod walk;

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::flist::{FileEntry, compare_file_entries};

use super::GeneratorContext;

#[cfg(all(unix, test))]
pub(super) use self::entry::rdev_to_major_minor;

impl GeneratorContext {
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
        // upstream: stats.flist_buildtime
        self.timing.flist_build_start = Some(Instant::now());

        info_log!(Flist, 1, "building file list...");
        self.clear_file_list();

        // upstream: flist.c:2192 - pre-allocate FLIST_START pointer slots
        const FLIST_START: usize = 4096;
        self.file_list.reserve(FLIST_START);
        self.full_paths.reserve(FLIST_START);

        for base_path in base_paths {
            self.walk_path(base_path, base_path.clone())?;
        }

        // upstream: flist.c:f_name_cmp() - sort both arrays via indirect permutation.
        // --qsort uses unstable sort (flist.c:2991).
        {
            let _t = PhaseTimer::new("file-list-sort");
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
        }

        // upstream: hlink.c:match_hard_links() - must be called after sort
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

        // upstream: flist.c:2287 - emit "." with XMIT_TOP_DIR for the root
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

        // Emit implied parent directory entries for files-from paths that
        // contain subdirectories. Without these entries the receiver cannot
        // create the parent directories needed for nested files.
        // upstream: flist.c:send_implied_dirs() - creates directory entries
        // for every intermediate path component of a --files-from entry.
        let mut emitted_dirs: HashSet<PathBuf> = HashSet::new();
        for path in file_paths {
            if let Ok(rel) = path.strip_prefix(base_dir) {
                // Walk each ancestor of the relative path and emit a
                // directory entry when we haven't seen it yet.
                let mut ancestor = PathBuf::new();
                for component in rel.parent().into_iter().flat_map(Path::components) {
                    ancestor.push(component);
                    if emitted_dirs.contains(&ancestor) {
                        continue;
                    }
                    let full = base_dir.join(&ancestor);
                    if let Ok(meta) = std::fs::symlink_metadata(&full) {
                        if meta.is_dir() {
                            if let Ok(entry) = self.create_entry(&full, ancestor.clone(), &meta) {
                                self.push_file_item(entry, full);
                            }
                        }
                    }
                    emitted_dirs.insert(ancestor.clone());
                }
            }
        }

        // Walk each listed file using the shared base directory so that
        // relative paths are computed correctly (e.g., "hello.txt" instead
        // of an empty string).
        for path in file_paths {
            self.walk_path(base_dir, path.clone())?;
        }

        // upstream: flist.c:f_name_cmp() - sort via indirect permutation
        {
            let _t = PhaseTimer::new("file-list-sort");
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
        }

        // upstream: hlink.c:match_hard_links() - must be called after sort
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
}

/// Applies a source-based permutation to two parallel slices in-place.
///
/// Reorders elements according to `source_indices` using cycle-following with
/// only swaps (no cloning). Used after sorting via indirect permutation to
/// reorder `file_list` and `full_paths` simultaneously.
///
/// The permutation is inverted first (`source_indices[i] = j` becomes
/// `dest_perm[j] = i`) so that cycle-following can use in-place swaps.
///
/// O(n) time and O(n) space for the inverse permutation.
///
/// # Upstream Reference
///
/// - `flist.c:f_name_cmp()` - upstream sorts the file list in-place;
///   we sort via indirect permutation to avoid O(n) clones of `FileEntry`.
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
