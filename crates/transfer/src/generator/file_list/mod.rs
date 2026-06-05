//! File list building, walking, and sorting for the generator role.
//!
//! Implements filesystem scanning (`walk_path`), `FileEntry` construction
//! (`create_entry`), hardlink index assignment, UID/GID collection, and
//! INC_RECURSE partitioning into per-directory segments.
//!
//! # Submodules
//!
//! - `batch_stat` - Parallel metadata resolution for directory children
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

mod batch_stat;
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

        let relative_paths = self.config.flags.relative;
        // upstream: flist.c:send_implied_dirs() - every parent directory of a
        // --relative source must be present in the file list so the receiver
        // can find it via flist_find_name() (generator.c:1313). We track
        // emitted ancestors across sources to avoid duplicate entries.
        let mut implied_ancestors: HashSet<PathBuf> = HashSet::new();
        for base_path in base_paths {
            // upstream: flist.c:2316 - --relative splits on "/./" so the dir
            // before the anchor is the base and everything after is the
            // transmitted relative name. Without an anchor the entire path is
            // the relative name (with the leading "/" stripped post-sort).
            let (base, path) = if relative_paths {
                relative_walk_base(base_path)
            } else {
                (base_path.clone(), base_path.clone())
            };
            // upstream: flist.c:2254-2272 - pre-stat each top-level source and
            // apply missing_args handling. Separates "source never existed" from
            // "source vanished during recursive walk".
            if !self.try_walk_source_entry(&base, &path)? {
                continue;
            }
            if relative_paths {
                self.emit_implied_parents(&base, &path, &mut implied_ancestors)?;
            }
        }

        // upstream: flist.c:f_name_cmp() - sort both arrays via indirect permutation.
        // --qsort uses unstable sort (flist.c:2991).
        {
            let _t = PhaseTimer::new("file-list-sort");
            let file_list_slice = self.file_list.as_slice();
            let mut indices: Vec<usize> = {
                let len = file_list_slice.len();
                let mut v = Vec::with_capacity(len);
                v.extend(0..len);
                v
            };
            let cmp = |&a: &usize, &b: &usize| {
                compare_file_entries(&file_list_slice[a], &file_list_slice[b])
            };
            if self.config.qsort {
                indices.sort_unstable_by(cmp);
            } else {
                indices.sort_by(cmp);
            }

            // Apply permutation in-place using cycle-following algorithm.
            // This avoids cloning every element - O(n) swaps instead of O(n) clones.
            let legacy = self.file_list.as_mut_vec();
            apply_permutation_in_place(legacy.as_mut_slice(), &mut self.full_paths, indices);
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
                dot_entry.set_top_dir(true);
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
            // upstream: flist.c:2254-2272 - pre-stat each --files-from entry
            // and apply missing_args handling before walk_path. This separates
            // "source never existed" (ENOENT at flist time) from "source vanished
            // during recursive walk" (ENOENT during child traversal).
            if !self.try_walk_source_entry(base_dir, path)? {
                continue;
            }
        }

        // upstream: flist.c:f_name_cmp() - sort via indirect permutation
        {
            let _t = PhaseTimer::new("file-list-sort");
            let file_list_slice = self.file_list.as_slice();
            let mut indices: Vec<usize> = {
                let len = file_list_slice.len();
                let mut v = Vec::with_capacity(len);
                v.extend(0..len);
                v
            };
            let cmp = |&a: &usize, &b: &usize| {
                compare_file_entries(&file_list_slice[a], &file_list_slice[b])
            };
            if self.config.qsort {
                indices.sort_unstable_by(cmp);
            } else {
                indices.sort_by(cmp);
            }

            let legacy = self.file_list.as_mut_vec();
            apply_permutation_in_place(legacy.as_mut_slice(), &mut self.full_paths, indices);
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

    /// Emits a directory entry for every implied ancestor of a `--relative`
    /// source path between `base` and `path`.
    ///
    /// Without inc-recurse, upstream rsync requires every parent directory of
    /// a relative source to be present in the file list so the receiver's
    /// `flist_find_name()` lookup at `generator.c:1313` succeeds. This walks
    /// from the path closest to `base` down to the source itself (exclusive),
    /// stat-ing each ancestor and recording it once via `implied_seen` to
    /// deduplicate across multiple source arguments.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1901-1980` - `send_implied_dirs()`
    /// - `generator.c:1300-1315` - parent dir presence check
    fn emit_implied_parents(
        &mut self,
        base: &Path,
        path: &Path,
        implied_seen: &mut HashSet<PathBuf>,
    ) -> io::Result<()> {
        let relative = path.strip_prefix(base).unwrap_or(path);
        let parent = match relative.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => return Ok(()),
        };

        // Build ancestor list from shallowest to deepest, excluding the
        // source path itself (which walk_path will record).
        let mut ancestors: Vec<PathBuf> = Vec::new();
        let mut current = parent.to_path_buf();
        loop {
            ancestors.push(current.clone());
            match current.parent() {
                Some(p) if !p.as_os_str().is_empty() => current = p.to_path_buf(),
                _ => break,
            }
        }

        for relative_ancestor in ancestors.into_iter().rev() {
            if !implied_seen.insert(relative_ancestor.clone()) {
                continue;
            }
            let full = base.join(&relative_ancestor);
            // upstream: flist.c:1949 - `copy_links = 1` is set before
            // emitting implied parents, so stat() follows symlinks. On
            // macOS /var is a symlink to /private/var; using
            // symlink_metadata would skip it (is_dir() false for a
            // symlink), breaking the ancestor chain.
            let meta = match std::fs::metadata(&full) {
                Ok(m) if m.is_dir() => m,
                _ => continue,
            };
            if let Ok(entry) = self.create_entry(&full, relative_ancestor, &meta) {
                self.push_file_item(entry, full);
            }
        }

        Ok(())
    }
}

/// Splits a source path for `--relative` mode into (base, full path).
///
/// Mirrors upstream rsync's `--relative` handling in `flist.c:2316-2350`:
///
/// - When the path contains `/./`, everything before the anchor becomes the
///   base (treated as `dir` upstream) and everything after becomes the
///   transmitted relative name.
/// - Without an anchor, the entire path is the relative name. Absolute paths
///   keep their root; the receiver strips the leading `/` post-sort
///   (`flist.c:3071-3084`). Relative paths use `.` as the base so
///   `strip_prefix` yields the original path verbatim.
///
/// The returned `base` is what `walk_path` strips from each child path to
/// compute its wire-side relative name.
fn relative_walk_base(path: &Path) -> (PathBuf, PathBuf) {
    // upstream: flist.c:2316 - `if ((p = strstr(fbuf, "/./")) != NULL)`
    if let Some(anchor) = find_dot_dir_anchor(path) {
        let path_str = path.as_os_str().to_string_lossy();
        let (head, tail) = path_str.split_at(anchor);
        // Skip the "/./" separator (3 chars) and any extra leading slashes.
        let rest = tail[3..].trim_start_matches('/');
        let base = if head.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(head)
        };
        let full = if rest.is_empty() {
            base.clone()
        } else {
            base.join(rest)
        };
        return (base, full);
    }

    // upstream: flist.c:2329 - no "/./" anchor: the entire path is the
    // relative name. Use "/" as base for absolute paths (the leading slash is
    // stripped by the receiver per flist.c:3071) and "." for relative paths.
    let base = if path.has_root() {
        PathBuf::from("/")
    } else {
        PathBuf::from(".")
    };
    (base, path.to_path_buf())
}

/// Locates the byte offset of `/./` in a path, used as the `--relative`
/// anchor separator.
fn find_dot_dir_anchor(path: &Path) -> Option<usize> {
    let s = path.as_os_str().to_str()?;
    s.find("/./")
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
