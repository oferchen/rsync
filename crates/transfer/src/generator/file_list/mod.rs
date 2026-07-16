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
mod iconv;
mod inc_recurse;
mod walk;

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::flist::FileEntry;

use super::GeneratorContext;

#[cfg(all(unix, test))]
pub(super) use self::entry::rdev_to_major_minor;

/// Marks a directory entry as an implied parent directory on the wire.
///
/// upstream: flist.c:1949 - `send_implied_dirs()` sets each implied parent's
/// flags to `(flags | FLAG_IMPLIED_DIR) & ~(FLAG_TOP_DIR | FLAG_CONTENT_DIR)`,
/// clearing CONTENT_DIR so the receiver does NOT scan the dir for `--delete`.
/// upstream: flist.c:2419 - the `--files-from`/`--relative` transfer-root `.`
/// is sent the same way (`& ~FLAG_CONTENT_DIR`). At encode time (flist.c:426)
/// a dir with FLAG_IMPLIED_DIR and no FLAG_CONTENT_DIR serializes as
/// `XMIT_TOP_DIR | XMIT_NO_CONTENT_DIR`, which the receiver decodes back to
/// FLAG_IMPLIED_DIR (flist.c:1117-1118) - never FLAG_CONTENT_DIR. In oc's flat
/// encoding that wire pair is `top_dir = true` + `content_dir = false`
/// (write/xflags.rs:93,284). A real content dir keeps `content_dir = true`.
///
/// Marking implied parents as content dirs (the old default) makes a real
/// upstream receiver scan them for `--delete` and over-delete stale files
/// upstream preserves (data loss).
fn mark_implied_dir(entry: &mut FileEntry) {
    entry.set_top_dir(true);
    entry.set_content_dir(false);
}

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
        self.source_bases.reserve(FLIST_START);

        let relative_paths = self.config.flags.relative;
        // upstream: flist.c:send_implied_dirs() - every parent directory of a
        // --relative source must be present in the file list so the receiver
        // can find it via flist_find_name() (generator.c:1313). We track
        // emitted ancestors across sources to avoid duplicate entries.
        let mut implied_ancestors: HashSet<PathBuf> = HashSet::new();
        for base_path in base_paths {
            // upstream: flist.c:2338-2349 - non-relative mode splits each
            // positional on the LAST `/`: `dir = strrchr(fbuf, '/')` becomes
            // the parent and `fn` becomes the basename, then `chdir(dir)`
            // walks `fn`. This makes the wire-side relative names carry the
            // source basename (e.g. `foo` and `foo/one` for source
            // `<mod>/foo`) instead of the post-strip-prefix names (which
            // would be empty for the source dir and `one` for its child,
            // mismatching upstream's wire output and tripping the
            // receiver's `rejecting unrequested file-list name` check).
            // upstream: flist.c:2316 - --relative additionally honours the
            // `/./` anchor and emits implied parent directories.
            let (base, path) = if relative_paths {
                relative_walk_base(base_path)
            } else {
                non_relative_walk_base(base_path)
            };
            // upstream: flist.c:2254-2272 - pre-stat each top-level source and
            // apply missing_args handling. Separates "source never existed" from
            // "source vanished during recursive walk".
            if !self.try_walk_source_entry(&base, &path)? {
                continue;
            }
            // upstream: flist.c:2257-2258 - `if (relative_paths &&
            // protocol_version >= 30) implied_dirs = 1;` forces the sender to
            // emit flagged implied parent dirs at protocol >= 30 regardless of
            // --no-implied-dirs. At protocol < 30 the flag is honoured
            // (flist.c:2468 `else if (implied_dirs && ...)`), so
            // --no-implied-dirs omits the implied parents from the flist and the
            // receiver recreates them via make_path (generator.c:1317) without
            // their source metadata. Mirror both: emit when implied dirs are on
            // OR the protocol forces them.
            if relative_paths && (!self.config.flags.no_implied_dirs || self.protocol.as_u8() >= 30)
            {
                self.emit_implied_parents(&base, &path, &mut implied_ancestors)?;
            }
        }

        // upstream: flist.c:1614-1638 send_file1() - drop entries whose names
        // cannot be strictly transcoded under --iconv before ndx assignment and
        // INC_RECURSE segmentation, so sender/receiver ndx values stay aligned.
        self.drop_unconvertible_entries();

        // upstream: flist.c:f_name_cmp() - sort both arrays via indirect permutation.
        // --qsort uses unstable sort (flist.c:2991).
        {
            let _t = PhaseTimer::new("file-list-sort");
            self.file_list
                .sort_with_parallel(&mut self.source_bases, self.config.qsort);
        }

        // upstream: flist.c:3031-3042 flist_sort_and_clean() - a non-incremental
        // sender does NOT run the duplicate-clean pass; it transmits every entry
        // as-is so the receiver's in-place tombstones keep both sides' NDX
        // numbering aligned with this full array. Only under INC_RECURSE does the
        // sender clean each sub-list (matching the receiver's identical pass).
        // A list with no duplicate names is unchanged either way, so this is a
        // no-op for normal transfers.
        let inc_recurse = self.inc_recurse();
        self.file_list
            .dedup_with_parallel(&mut self.source_bases, true, inc_recurse);

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

    /// Builds a file list from `--files-from` entries.
    ///
    /// Unlike [`build_file_list`](Self::build_file_list), which treats each
    /// positional argument as its own base for `walk_path`, this method honors
    /// the per-entry base produced by
    /// `split_files_from_entry`.
    /// Each entry's wire-side relative name is computed by stripping its own
    /// `base`, matching upstream rsync's `chdir(dir)` + transmit-`fn` split
    /// (`flist.c:2316-2330`). The `base_dir` argument is the source argument
    /// shared by entries without a `/./` anchor and, in `--relative` mode with
    /// a leading `./` anchor, roots the implied transfer-root `.` entry.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2240-2264` - `change_dir(argv[0])` then read relative filenames
    /// - `flist.c:2316-2330` - per-entry `/./` anchor split
    /// - `flist.c:2368-2419` - `implied_dot_dir` gates the transfer-root `.`
    ///   emission via `send_file_name(".", ..., FLAG_IMPLIED_DIR & ~FLAG_CONTENT_DIR, ...)`
    pub fn build_file_list_with_base(
        &mut self,
        base_dir: &Path,
        entries: &[super::filters::FilesFromEntry],
    ) -> io::Result<usize> {
        self.timing.flist_build_start = Some(Instant::now());

        info_log!(Flist, 1, "building file list from --files-from...");
        self.clear_file_list();

        const FLIST_START: usize = 4096;
        self.file_list.reserve(FLIST_START);
        self.source_bases.reserve(FLIST_START);

        // upstream: flist.c:2368-2419 - the transfer-root `.` entry is emitted
        // ONLY in `--relative` mode and ONLY when some `--files-from` entry has
        // a leading `./` anchor (`implied_dot_dir`). Upstream emits it via
        // `send_file_name(".", ..., (flags | FLAG_IMPLIED_DIR) & ~FLAG_CONTENT_DIR, ...)`
        // (flist.c:2419): FLAG_IMPLIED_DIR set, FLAG_TOP_DIR NOT set,
        // FLAG_CONTENT_DIR cleared. Such an entry does NOT scope `--delete` over
        // the destination root. A plain list with no `./` anchor emits NO `.`
        // entry at all.
        //
        // Emitting an unconditional `.` here (the old behaviour) produced a
        // spurious `.d ./` itemize row on the receiver and, worse, with
        // `--delete` its TOP_DIR + CONTENT_DIR flags scoped deletion over the
        // destination root, deleting stale root-level files that upstream
        // preserves (data loss).
        //
        // Wire encoding: FLAG_IMPLIED_DIR with FLAG_CONTENT_DIR cleared
        // serializes as XMIT_TOP_DIR | XMIT_NO_CONTENT_DIR (flist.c:426-427);
        // the receiver decodes that pair back to FLAG_IMPLIED_DIR
        // (flist.c:1117-1118), never FLAG_TOP_DIR/FLAG_CONTENT_DIR. In oc's flat
        // encoding that is `set_top_dir(true)` + `set_content_dir(false)`
        // (`calculate_basic_flags`/`calculate_directory_flags`).
        //
        // A leading-`./` entry never has `path == base` (a bare `.`/`./` DOTDIR
        // does, but carries `implied_dot == false`), so the two never collide
        // on a duplicate root `.`.
        let emit_implied_root_dot =
            self.config.flags.relative && entries.iter().any(|e| e.implied_dot);
        if emit_implied_root_dot {
            if let Ok(meta) = std::fs::symlink_metadata(base_dir) {
                if meta.is_dir() {
                    let mut dot_entry = self.create_entry(base_dir, PathBuf::from("."), &meta)?;
                    mark_implied_dir(&mut dot_entry);
                    self.push_file_item(dot_entry, base_dir.to_path_buf());
                }
            }
        }

        // Pre-populate the explicit-directory set with every --files-from
        // entry that is itself a directory. The implied-parent loop below
        // skips emission for entries already in this set so the explicit
        // top-level walk owns their emission. Keys are the (base, wire-relative)
        // pair so two entries that resolve to the same filesystem path but
        // through different `/./` anchors do not collide on the wire-side
        // dedup map.
        let mut explicit_dirs: HashSet<(PathBuf, PathBuf)> = HashSet::new();
        for entry in entries {
            if let Ok(rel) = entry.path.strip_prefix(&entry.base) {
                if rel.as_os_str().is_empty() {
                    continue;
                }
                if let Ok(meta) = std::fs::symlink_metadata(&entry.path) {
                    if meta.is_dir() {
                        explicit_dirs.insert((entry.base.clone(), rel.to_path_buf()));
                    }
                }
            }
        }

        // Emit implied parent directory entries for files-from paths that
        // contain subdirectories. Without these entries the receiver cannot
        // create the parent directories needed for nested files.
        // upstream: flist.c:send_implied_dirs() - creates directory entries
        // for every intermediate path component of a --files-from entry.
        //
        // `emitted_dirs` starts with the explicit-dir pre-population so a
        // parent that is ALSO an explicit --files-from entry is not emitted
        // here (the top-level walk below owns its emission). The loop adds
        // every purely implied ancestor it pushes; the difference set
        // `implied_only_dirs` is later consulted by
        // `try_walk_source_entry_dedup` to suppress the duplicate top-level
        // walk that would re-emit an implied parent.
        let mut emitted_dirs: HashSet<(PathBuf, PathBuf)> = explicit_dirs.clone();
        // upstream: options.c:2207-2208 - `if (!relative_paths) implied_dirs = 0;`.
        // Under --no-relative (relative_paths == 0) the sender FLATTENS every
        // --files-from entry to its transmitted name and emits NO implied parent
        // directories (flist.c:2468 gates the send on `implied_dirs`, which is
        // forced off). Without this gate oc emits an intermediate `sub` dir that
        // an upstream receiver rejects as an unrequested file-list name (exit 4).
        // upstream: flist.c:2257-2258 - `if (relative_paths && protocol_version
        // >= 30) implied_dirs = 1;` forces flagged implied parent dirs at
        // protocol >= 30 regardless of --no-implied-dirs; at protocol < 30 the
        // flag is honoured (flist.c:2468 `else if (implied_dirs && ...)`), so
        // --no-implied-dirs omits the implied parents from the --files-from
        // flist and the receiver recreates them via make_path (generator.c:1317)
        // without their source metadata. Mirror the same gate as the positional
        // path (build_file_list): emit only in relative mode, and then when
        // implied dirs are on OR the protocol forces them. When suppressed,
        // `emitted_dirs` stays equal to `explicit_dirs`, so `implied_only_dirs`
        // below is empty and the explicit top-level walk is left untouched
        // (dedup unchanged).
        if self.config.flags.relative
            && (!self.config.flags.no_implied_dirs || self.protocol.as_u8() >= 30)
        {
            for entry in entries {
                if let Ok(rel) = entry.path.strip_prefix(&entry.base) {
                    // Walk each ancestor of the relative path and emit a
                    // directory entry when we haven't seen it yet.
                    let mut ancestor = PathBuf::new();
                    for component in rel.parent().into_iter().flat_map(Path::components) {
                        ancestor.push(component);
                        let key = (entry.base.clone(), ancestor.clone());
                        if emitted_dirs.contains(&key) {
                            continue;
                        }
                        let full = entry.base.join(&ancestor);
                        if let Ok(meta) = std::fs::symlink_metadata(&full) {
                            if meta.is_dir() {
                                if let Ok(mut file_entry) =
                                    self.create_entry(&full, ancestor.clone(), &meta)
                                {
                                    // upstream: flist.c:1949 - implied parents clear
                                    // FLAG_CONTENT_DIR so a real upstream receiver does
                                    // not scan them for --delete (over-delete data loss).
                                    mark_implied_dir(&mut file_entry);
                                    self.push_file_item(file_entry, full);
                                }
                            }
                        }
                        emitted_dirs.insert(key);
                    }
                }
            }
        }

        // Directories emitted purely as implied parents of some other entry
        // (i.e. not also listed explicitly in --files-from). The top-level
        // walk skips these so we do not produce a duplicate file-list entry
        // that upstream's `implied_filter_list` check (flist.c:998) would
        // reject as "unrequested". Explicit --files-from dirs remain walkable
        // so their recursive contents continue to flow normally.
        let implied_only_dirs: HashSet<(PathBuf, PathBuf)> =
            emitted_dirs.difference(&explicit_dirs).cloned().collect();

        // Walk each listed file using its own per-entry base so that the
        // wire-side relative name reflects the `/./` anchor split (e.g.
        // `from/./dir/subdir` transmits as `dir/subdir`, not
        // `from/dir/subdir`).
        for entry in entries {
            // upstream: flist.c:2254-2272 - pre-stat each --files-from entry
            // and apply missing_args handling before walk_path. This separates
            // "source never existed" (ENOENT at flist time) from "source vanished
            // during recursive walk" (ENOENT during child traversal).
            let scoped: HashSet<PathBuf> = implied_only_dirs
                .iter()
                .filter(|(b, _)| b == &entry.base)
                .map(|(_, rel)| rel.clone())
                .collect();
            if !self.try_walk_source_entry_dedup(&entry.base, &entry.path, Some(&scoped))? {
                continue;
            }

            // upstream: flist.c:2329 - SLASH_ENDING_NAME / DOTDIR_NAME entries
            // recurse into their children even when global `-r` is off. Plain
            // `try_walk_source_entry_dedup` honours the global `recursive` flag
            // so the trailing-slash directories would otherwise stop at the
            // entry itself. Re-scan the directory here so the receiver sees
            // the listed dir's contents (`from/./dir/subdir/subsubdir2/` must
            // emit `bin-lt-list`, etc.). DOTDIR entries (`from/./` and
            // `from/.`) produce `entry.path == entry.base`; they still need
            // the rescan because `flags.recursive` is cleared whenever
            // `--files-from` is active (upstream `options.c:2189`), so
            // `walk_path_with_metadata` would emit only the root entry.
            if entry.recurse {
                if let Ok(meta) = std::fs::symlink_metadata(&entry.path) {
                    if meta.is_dir() {
                        self.scan_files_from_marker_dir(&entry.base, &entry.path)?;
                    }
                }
            }
        }

        // upstream: flist.c:f_name_cmp() - sort via indirect permutation
        {
            let _t = PhaseTimer::new("file-list-sort");
            self.file_list
                .sort_with_parallel(&mut self.source_bases, self.config.qsort);
        }

        // upstream: flist.c:3031-3042 flist_sort_and_clean() - the --files-from
        // build path. A non-incremental sender transmits duplicates as-is so the
        // receiver's in-place tombstones keep NDX aligned; only under INC_RECURSE
        // does the sender clean each sub-list. No-op when there are no duplicate
        // names.
        let inc_recurse = self.inc_recurse();
        self.file_list
            .dedup_with_parallel(&mut self.source_bases, true, inc_recurse);

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
            if let Ok(mut entry) = self.create_entry(&full, relative_ancestor, &meta) {
                // upstream: flist.c:1949 - implied parents clear FLAG_CONTENT_DIR
                // so a real upstream receiver does not scan them for --delete.
                mark_implied_dir(&mut entry);
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

/// Picks the `(base, path)` pair for a non-`--relative` positional, matching
/// upstream `flist.c:2338-2349`: split the path on its LAST `/`, take the
/// prefix as the base directory and the suffix as the file name. The full
/// path is preserved so callers can pass it to `link_stat`, but `base` is
/// what `walk_path_with_metadata` strips to compute the wire-side relative
/// name. For a path with no `/` separator (i.e. a bare basename), the base
/// is `.` so `strip_prefix` is a no-op and the entry surfaces under its
/// own name.
///
/// Examples:
///   * `/srv/mod/foo`  -> base=`/srv/mod`,  path=`/srv/mod/foo`
///   * `/srv/mod/foo/` -> base=`/srv/mod`,  path=`/srv/mod/foo/`
///   * `/srv/mod/`     -> base=`/srv/mod/`, path=`/srv/mod/`     (dotdir)
///   * `/`             -> base=`/`,         path=`/`             (dotdir)
///   * `foo`           -> base=`.`,         path=`foo`
fn non_relative_walk_base(path: &Path) -> (PathBuf, PathBuf) {
    // Upstream's DOTDIR_NAME branch (flist.c:2312-2322) preserves a
    // trailing slash to signal "transfer the contents only". Preserve
    // base == path so `walk_path_with_metadata`'s `relative.is_empty()`
    // branch still emits `.` for the source root.
    let s = path.as_os_str();
    let bytes = s.as_encoded_bytes();
    if bytes.last() == Some(&b'/') {
        return (path.to_path_buf(), path.to_path_buf());
    }
    // `Path::parent()` returns the parent directory or `None` for a path
    // whose final component is the root or a bare basename. The bare-name
    // case is normalised to `.` so `strip_prefix` becomes a no-op and the
    // entry surfaces under its own name, matching upstream's
    // `fn = fbuf; dir = NULL -> chdir(NULL)` no-op.
    let parent = path.parent().map(|p| {
        if p.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            p.to_path_buf()
        }
    });
    match parent {
        Some(base) => (base, path.to_path_buf()),
        None => (path.to_path_buf(), path.to_path_buf()),
    }
}

#[cfg(test)]
pub(super) use protocol::flist::apply_permutation_in_place;
