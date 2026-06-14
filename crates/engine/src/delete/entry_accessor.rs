//! `FileEntryAccessor`-generic helpers for the delete pipeline (RSS-A.7.h).
//!
//! Each function in this module mirrors a concrete-`FileEntry` consumer in
//! the delete pipeline but accepts `T: FileEntryAccessor` instead, enabling
//! the flat arena-backed `FlatFileEntry` to be used interchangeably with
//! the legacy `FileEntry` representation.
//!
//! # Consumer sites migrated
//!
//! | Original site | Generic helper |
//! |---|---|
//! | `extras::segment_basenames` | [`segment_basenames_generic`] |
//! | `extras::compute_extras` | [`compute_extras_generic`] |
//! | `extras::compute_extras_with_cohorts` | [`compute_extras_with_cohorts_generic`] |
//! | `traversal::DirTraversalCursor::observe_segment` | `observe_segment_generic` |
//! | `traversal::DirTraversalCursor::observe_segment` | `collect_child_dirs_generic` |
//! | `cohort_index::CohortIndex::build_from_flist_segment` | [`GenericCohortIndex::build_from_entries`] |
//!
//! # Feature gate
//!
//! This module is compiled only when the `flat-flist` Cargo feature is
//! active. Production builds that use the legacy `Vec<FileEntry>` path
//! are byte-identical to the pre-migration code.
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
//!   (`delete_in_dir`): set-subtraction and traversal logic.
//! - `target/interop/upstream-src/rsync-3.4.1/hlink.c:59-65`
//!   (`init_hard_links`): cohort index construction.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::flist::FileEntryAccessor;
use rustc_hash::FxHashMap;

use super::cohort_index::CohortIndex;
use super::extras::classify;
use super::plan::{DeleteEntry, HardlinkCohortId};
use super::traversal::DirTraversalCursor;

/// Collects the leaf basenames from a slice of accessor-backed entries into
/// a hash set keyed by [`OsString`].
///
/// Mirrors `super::extras::segment_basenames` but works with any
/// `T: FileEntryAccessor`. The accessor's [`FileEntryAccessor::name`]
/// returns the full relative path string; we extract the leaf component
/// the same way the concrete version uses `entry.path().file_name()`.
///
/// Entries whose name is empty or has no leaf component are skipped,
/// matching the upstream `flist_find` semantics that never match the
/// empty path.
pub fn segment_basenames_generic<T: FileEntryAccessor>(entries: &[T]) -> HashSet<OsString> {
    let mut set = HashSet::with_capacity(entries.len());
    for entry in entries {
        let name = entry.name();
        if name.is_empty() {
            continue;
        }
        let path = Path::new(name);
        if let Some(basename) = path.file_name() {
            set.insert(basename.to_os_string());
        }
    }
    set
}

/// Extracts child directory basenames from an accessor-backed entry slice,
/// producing fully-qualified paths relative to `parent_dir`.
///
/// Mirrors the directory-filtering and basename extraction in
/// [`super::traversal::DirTraversalCursor::observe_segment`] but accepts
/// `T: FileEntryAccessor` instead of `&[FileEntry]`.
///
/// Only entries where [`FileEntryAccessor::is_dir`] returns `true` are
/// included. The returned paths are of the form `parent_dir.join(basename)`
/// unless `parent_dir` is empty or `.`, in which case the bare basename is
/// used. Duplicate basenames within the same call are deduplicated.
pub fn collect_child_dirs_generic<T: FileEntryAccessor>(
    parent_dir: &Path,
    entries: &[T],
) -> Vec<PathBuf> {
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in entries {
        if !entry.is_dir() {
            continue;
        }
        let name_str = entry.name();
        if name_str.is_empty() {
            continue;
        }
        let basename = match Path::new(name_str).file_name() {
            Some(b) => b,
            None => continue,
        };
        if basename.is_empty() {
            continue;
        }
        let full = if parent_dir.as_os_str().is_empty() || parent_dir == Path::new(".") {
            PathBuf::from(basename)
        } else {
            parent_dir.join(basename)
        };
        if !children.iter().any(|p| p == &full) {
            children.push(full);
        }
    }
    children
}

/// Read-only cohort index built from accessor-backed entries.
///
/// Mirrors [`super::CohortIndex`] but is constructed from a slice of
/// `T: FileEntryAccessor` entries. The resulting index has the same API
/// surface and is wrapped in [`Arc`] for sharing across rayon workers.
///
/// See [`super::cohort_index::CohortIndex`] for field-level documentation.
#[derive(Debug, Default)]
pub struct GenericCohortIndex {
    /// Basename -> cohort id.
    by_name: HashMap<OsString, HardlinkCohortId>,
    /// `(dev, ino)` -> cohort id (protocol-29 entries only).
    by_dev_ino: FxHashMap<(u64, u64), HardlinkCohortId>,
    /// Cohort id -> source-side ref count.
    cohort_sizes: FxHashMap<HardlinkCohortId, u32>,
}

impl GenericCohortIndex {
    /// Builds a cohort index from a slice of accessor-backed entries.
    ///
    /// Two-pass algorithm identical to
    /// [`super::CohortIndex::build_from_flist_segment`]:
    ///
    /// 1. Identify leaders (`hardlink_idx() == Some(u32::MAX)`).
    /// 2. Attach followers to their leader's cohort.
    ///
    /// The cohort id is minted from the leader's position in the slice
    /// so it is stable across both leader and followers.
    #[must_use]
    pub fn build_from_entries<T: FileEntryAccessor>(entries: &[T]) -> Arc<Self> {
        let mut leader_for_idx: FxHashMap<u32, HardlinkCohortId> = FxHashMap::default();
        let mut by_name: HashMap<OsString, HardlinkCohortId> = HashMap::new();
        let mut by_dev_ino: FxHashMap<(u64, u64), HardlinkCohortId> = FxHashMap::default();
        let mut cohort_sizes: FxHashMap<HardlinkCohortId, u32> = FxHashMap::default();

        // Pass 1: identify leaders.
        for (position, entry) in entries.iter().enumerate() {
            if entry.hardlink_idx() == Some(u32::MAX) {
                let cohort = HardlinkCohortId::new(position as u32);
                leader_for_idx.insert(position as u32, cohort);
                Self::record(
                    entry,
                    cohort,
                    &mut by_name,
                    &mut by_dev_ino,
                    &mut cohort_sizes,
                );
            }
        }

        // Pass 2: attach followers.
        for entry in entries.iter() {
            let Some(leader_ndx) = entry.hardlink_idx() else {
                continue;
            };
            if leader_ndx == u32::MAX {
                continue;
            }
            let Some(&cohort) = leader_for_idx.get(&leader_ndx) else {
                // Cross-segment follower; skip.
                continue;
            };
            Self::record(
                entry,
                cohort,
                &mut by_name,
                &mut by_dev_ino,
                &mut cohort_sizes,
            );
        }

        Arc::new(Self {
            by_name,
            by_dev_ino,
            cohort_sizes,
        })
    }

    /// Looks up the cohort id for a destination basename.
    #[must_use]
    pub fn cohort_of(&self, name: &std::ffi::OsStr) -> Option<HardlinkCohortId> {
        self.by_name.get(name).copied()
    }

    /// Looks up the cohort id by destination `(dev, ino)`.
    #[must_use]
    pub fn cohort_by_dev_ino(&self, dev: u64, ino: u64) -> Option<HardlinkCohortId> {
        self.by_dev_ino.get(&(dev, ino)).copied()
    }

    /// Returns the source-side ref count for a cohort.
    #[must_use]
    pub fn surviving_refs_in_cohort(&self, cohort: HardlinkCohortId) -> u32 {
        self.cohort_sizes.get(&cohort).copied().unwrap_or(0)
    }

    /// Returns the number of distinct cohorts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cohort_sizes.len()
    }

    /// Returns `true` when no cohorts were observed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cohort_sizes.is_empty()
    }

    /// Records one cohort member into the indexes.
    fn record<T: FileEntryAccessor>(
        entry: &T,
        cohort: HardlinkCohortId,
        by_name: &mut HashMap<OsString, HardlinkCohortId>,
        by_dev_ino: &mut FxHashMap<(u64, u64), HardlinkCohortId>,
        cohort_sizes: &mut FxHashMap<HardlinkCohortId, u32>,
    ) {
        let name_str = entry.name();
        if !name_str.is_empty() {
            if let Some(basename) = Path::new(name_str).file_name() {
                by_name.insert(basename.to_os_string(), cohort);
            }
        }
        if let (Some(dev), Some(ino)) = (entry.hardlink_dev(), entry.hardlink_ino()) {
            by_dev_ino.insert((dev as u64, ino as u64), cohort);
        }
        let count = cohort_sizes.entry(cohort).or_insert(0);
        *count = count.saturating_add(1);
    }
}

/// Lists `dest_dir`, subtracts every basename that appears in
/// `segment_entries`, and classifies each surviving entry by kind.
///
/// Generic counterpart of [`super::extras::compute_extras`] that accepts
/// any `T: FileEntryAccessor` for the segment entries, enabling the same
/// set-subtraction logic to work with both the legacy `FileEntry` and the
/// arena-backed `FlatFileEntry`.
///
/// The returned vector is unsorted. Callers wrap it in a
/// [`super::plan::DeletePlan`] and sort before publishing.
///
/// # Errors
///
/// Returns the I/O error from [`fs::read_dir`] or per-entry
/// `symlink_metadata` if `dest_dir` cannot be scanned.
pub fn compute_extras_generic<T: FileEntryAccessor>(
    dest_dir: &Path,
    segment_entries: &[T],
) -> io::Result<Vec<DeleteEntry>> {
    compute_extras_with_cohorts_generic(dest_dir, segment_entries, None)
}

/// Variant of [`compute_extras_generic`] that attaches a hardlink cohort
/// tag to each surviving entry whose destination basename matches a member
/// of the supplied [`CohortIndex`].
///
/// Generic counterpart of [`super::extras::compute_extras_with_cohorts`].
/// The cohort tag has no effect on the unlink decision itself; it exists
/// for itemize-line decoration and diagnostics.
///
/// `cohort_index = None` reproduces the original behaviour bit for bit.
///
/// # Errors
///
/// Same as [`compute_extras_generic`]: any I/O failure on `read_dir` or
/// per-entry `symlink_metadata` is surfaced to the caller.
pub fn compute_extras_with_cohorts_generic<T: FileEntryAccessor>(
    dest_dir: &Path,
    segment_entries: &[T],
    cohort_index: Option<&Arc<CohortIndex>>,
) -> io::Result<Vec<DeleteEntry>> {
    let segment_names = segment_basenames_generic(segment_entries);
    let read_dir = fs::read_dir(dest_dir)?;
    let mut extras = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let name = entry.file_name();
        if segment_names.contains(&name) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        let kind = classify(&metadata);
        let delete_entry = match cohort_index.and_then(|idx| idx.cohort_of(name.as_os_str())) {
            Some(cohort) => DeleteEntry::with_cohort(name, kind, cohort),
            None => DeleteEntry::new(name, kind),
        };
        extras.push(delete_entry);
    }
    Ok(extras)
}

impl DirTraversalCursor {
    /// Records the directory children observed in one flist segment,
    /// accepting any `T: FileEntryAccessor` instead of `&[FileEntry]`.
    ///
    /// Generic counterpart of [`DirTraversalCursor::observe_segment`].
    /// Only entries where [`FileEntryAccessor::is_dir`] returns `true` are
    /// kept. The stored list is re-sorted in `f_name_cmp` order after
    /// each call. Late observations after the parent has been advanced
    /// past are silently dropped.
    pub fn observe_segment_generic<T: FileEntryAccessor>(&mut self, dir: PathBuf, children: &[T]) {
        let entry = self.child_dirs_mut().entry(dir.clone()).or_default();
        for child in children {
            if !child.is_dir() {
                continue;
            }
            let name_str = child.name();
            if name_str.is_empty() {
                continue;
            }
            let basename = match Path::new(name_str).file_name() {
                Some(b) => b,
                None => continue,
            };
            if basename.is_empty() {
                continue;
            }
            let full = if dir.as_os_str().is_empty() || dir == Path::new(".") {
                PathBuf::from(basename)
            } else {
                dir.join(basename)
            };
            if !entry.iter().any(|p| p == &full) {
                entry.push(full);
            }
        }
        super::traversal::sort_paths_by_f_name_cmp(entry);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use protocol::flist::FileEntry;

    use super::*;

    #[test]
    fn basenames_empty_slice() {
        let entries: Vec<FileEntry> = Vec::new();
        let set = segment_basenames_generic(&entries);
        assert!(set.is_empty());
    }

    #[test]
    fn basenames_extracts_leaf_from_full_paths() {
        let entries = vec![
            FileEntry::new_file("sub/a".into(), 0, 0o644),
            FileEntry::new_file("sub/b".into(), 0, 0o644),
            FileEntry::new_file("c".into(), 0, 0o644),
        ];
        let set = segment_basenames_generic(&entries);
        assert_eq!(set.len(), 3);
        assert!(set.contains(&OsString::from("a")));
        assert!(set.contains(&OsString::from("b")));
        assert!(set.contains(&OsString::from("c")));
    }

    #[test]
    fn basenames_skips_empty_paths() {
        let entries = vec![
            FileEntry::new_file(PathBuf::new(), 0, 0o644),
            FileEntry::new_file("ok".into(), 0, 0o644),
        ];
        let set = segment_basenames_generic(&entries);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&OsString::from("ok")));
    }

    #[test]
    fn basenames_duplicates_collapsed() {
        let entries = vec![
            FileEntry::new_file("d/x".into(), 0, 0o644),
            FileEntry::new_file("e/x".into(), 0, 0o644),
        ];
        let set = segment_basenames_generic(&entries);
        // Both entries share basename "x"; the set should contain it once.
        assert_eq!(set.len(), 1);
        assert!(set.contains(&OsString::from("x")));
    }

    #[test]
    fn basenames_deeply_nested_paths() {
        let entries = vec![FileEntry::new_file("a/b/c/d/leaf.txt".into(), 0, 0o644)];
        let set = segment_basenames_generic(&entries);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&OsString::from("leaf.txt")));
    }

    #[test]
    fn child_dirs_empty_slice() {
        let entries: Vec<FileEntry> = Vec::new();
        let dirs = collect_child_dirs_generic(Path::new("root"), &entries);
        assert!(dirs.is_empty());
    }

    #[test]
    fn child_dirs_filters_non_directories() {
        let entries = vec![
            FileEntry::new_directory("root/sub".into(), 0o755),
            FileEntry::new_file("root/file.txt".into(), 0, 0o644),
            FileEntry::new_symlink("root/link".into(), PathBuf::from("target")),
        ];
        let dirs = collect_child_dirs_generic(Path::new("root"), &entries);
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], PathBuf::from("root/sub"));
    }

    #[test]
    fn child_dirs_deduplicates() {
        let entries = vec![
            FileEntry::new_directory("root/sub".into(), 0o755),
            FileEntry::new_directory("root/sub".into(), 0o755),
        ];
        let dirs = collect_child_dirs_generic(Path::new("root"), &entries);
        assert_eq!(dirs.len(), 1);
    }

    #[test]
    fn child_dirs_empty_parent_uses_bare_basename() {
        let entries = vec![FileEntry::new_directory("a".into(), 0o755)];
        let dirs = collect_child_dirs_generic(Path::new(""), &entries);
        assert_eq!(dirs, vec![PathBuf::from("a")]);
    }

    #[test]
    fn child_dirs_dot_parent_uses_bare_basename() {
        let entries = vec![FileEntry::new_directory("b".into(), 0o755)];
        let dirs = collect_child_dirs_generic(Path::new("."), &entries);
        assert_eq!(dirs, vec![PathBuf::from("b")]);
    }

    #[test]
    fn child_dirs_preserves_order() {
        let entries = vec![
            FileEntry::new_directory("root/c".into(), 0o755),
            FileEntry::new_directory("root/a".into(), 0o755),
            FileEntry::new_directory("root/b".into(), 0o755),
        ];
        let dirs = collect_child_dirs_generic(Path::new("root"), &entries);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("root/c"),
                PathBuf::from("root/a"),
                PathBuf::from("root/b"),
            ]
        );
    }

    fn leader(name: &str) -> FileEntry {
        let mut entry = FileEntry::new_file(PathBuf::from(name), 0, 0o644);
        entry.set_hardlink_idx(u32::MAX);
        entry
    }

    fn leader_with_dev_ino(name: &str, dev: i64, ino: i64) -> FileEntry {
        let mut entry = leader(name);
        entry.set_hardlink_dev(dev);
        entry.set_hardlink_ino(ino);
        entry
    }

    fn follower(name: &str, leader_idx: u32) -> FileEntry {
        let mut entry = FileEntry::new_file(PathBuf::from(name), 0, 0o644);
        entry.set_hardlink_idx(leader_idx);
        entry
    }

    fn follower_with_dev_ino(name: &str, leader_idx: u32, dev: i64, ino: i64) -> FileEntry {
        let mut entry = follower(name, leader_idx);
        entry.set_hardlink_dev(dev);
        entry.set_hardlink_ino(ino);
        entry
    }

    #[test]
    fn cohort_empty_segment() {
        let index = GenericCohortIndex::build_from_entries::<FileEntry>(&[]);
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn cohort_non_hardlinked_entries_yield_empty_index() {
        let entries = vec![
            FileEntry::new_file("a".into(), 0, 0o644),
            FileEntry::new_file("b".into(), 0, 0o644),
        ];
        let index = GenericCohortIndex::build_from_entries(&entries);
        assert!(index.is_empty());
        assert!(index.cohort_of(OsStr::new("a")).is_none());
    }

    #[test]
    fn cohort_single_leader() {
        let entries = vec![leader("solo")];
        let index = GenericCohortIndex::build_from_entries(&entries);
        let cohort = index
            .cohort_of(OsStr::new("solo"))
            .expect("leader registers by name");
        assert_eq!(cohort, HardlinkCohortId::new(0));
        assert_eq!(index.surviving_refs_in_cohort(cohort), 1);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn cohort_leader_and_followers_share_id() {
        let entries = vec![leader("leader"), follower("ref1", 0), follower("ref2", 0)];
        let index = GenericCohortIndex::build_from_entries(&entries);
        let cohort = index
            .cohort_of(OsStr::new("leader"))
            .expect("leader present");
        assert_eq!(index.cohort_of(OsStr::new("ref1")), Some(cohort));
        assert_eq!(index.cohort_of(OsStr::new("ref2")), Some(cohort));
        assert_eq!(index.surviving_refs_in_cohort(cohort), 3);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn cohort_cross_segment_follower_skipped() {
        let entries = vec![follower("orphan", 999)];
        let index = GenericCohortIndex::build_from_entries(&entries);
        assert!(index.is_empty());
        assert!(index.cohort_of(OsStr::new("orphan")).is_none());
    }

    #[test]
    fn cohort_dev_ino_lookup() {
        let entries = vec![
            leader_with_dev_ino("a", 5, 100),
            follower_with_dev_ino("b", 0, 5, 100),
        ];
        let index = GenericCohortIndex::build_from_entries(&entries);
        let cohort = index.cohort_by_dev_ino(5, 100).expect("dev/ino indexed");
        assert_eq!(index.cohort_of(OsStr::new("a")), Some(cohort));
        assert_eq!(index.cohort_of(OsStr::new("b")), Some(cohort));
    }

    #[test]
    fn cohort_dev_ino_none_when_fields_absent() {
        let entries = vec![leader("a"), follower("b", 0)];
        let index = GenericCohortIndex::build_from_entries(&entries);
        assert!(index.cohort_by_dev_ino(0, 0).is_none());
        assert!(index.cohort_of(OsStr::new("a")).is_some());
    }

    #[test]
    fn cohort_multiple_distinct_cohorts() {
        let entries = vec![
            leader("g1_leader"),
            follower("g1_member", 0),
            leader("g2_leader"),
            follower("g2_member", 2),
        ];
        let index = GenericCohortIndex::build_from_entries(&entries);
        let g1 = index.cohort_of(OsStr::new("g1_leader")).unwrap();
        let g2 = index.cohort_of(OsStr::new("g2_leader")).unwrap();
        assert_ne!(g1, g2);
        assert_eq!(index.cohort_of(OsStr::new("g1_member")), Some(g1));
        assert_eq!(index.cohort_of(OsStr::new("g2_member")), Some(g2));
        assert_eq!(index.surviving_refs_in_cohort(g1), 2);
        assert_eq!(index.surviving_refs_in_cohort(g2), 2);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn cohort_subdirectory_path_indexes_by_leaf() {
        let mut entry = FileEntry::new_file(PathBuf::from("sub/dir/leader.txt"), 0, 0o644);
        entry.set_hardlink_idx(u32::MAX);
        let index = GenericCohortIndex::build_from_entries(&[entry]);
        assert!(index.cohort_of(OsStr::new("leader.txt")).is_some());
        assert!(index.cohort_of(OsStr::new("sub/dir/leader.txt")).is_none());
    }

    #[test]
    fn cohort_empty_path_skipped() {
        let mut entry = FileEntry::new_file(PathBuf::new(), 0, 0o644);
        entry.set_hardlink_idx(u32::MAX);
        let index = GenericCohortIndex::build_from_entries(&[entry]);
        // Cohort exists by size tracking but no name lookup hits it.
        assert_eq!(index.len(), 1);
        assert!(index.cohort_of(OsStr::new("")).is_none());
    }

    #[test]
    fn cohort_generic_matches_concrete() {
        // Verify the generic index produces the same cohort assignments
        // as the concrete CohortIndex for an identical input.
        let entries = vec![leader("a"), follower("b", 0), leader("c"), follower("d", 2)];
        let generic = GenericCohortIndex::build_from_entries(&entries);
        let concrete = super::super::CohortIndex::build_from_flist_segment(&entries);

        // Same number of cohorts.
        assert_eq!(generic.len(), concrete.len());

        // Same cohort assignments by name.
        for name in ["a", "b", "c", "d"] {
            let g = generic.cohort_of(OsStr::new(name));
            let c = concrete.cohort_of(OsStr::new(name));
            assert_eq!(
                g, c,
                "cohort mismatch for entry '{name}': generic={g:?}, concrete={c:?}"
            );
        }

        // Same surviving ref counts.
        if let Some(cohort) = generic.cohort_of(OsStr::new("a")) {
            assert_eq!(
                generic.surviving_refs_in_cohort(cohort),
                concrete.surviving_refs_in_cohort(cohort),
            );
        }
    }

    #[test]
    fn cohort_arc_is_shareable() {
        let entries = vec![leader("a"), follower("b", 0)];
        let index = GenericCohortIndex::build_from_entries(&entries);
        let mut handles = Vec::new();
        for _ in 0..4 {
            let idx = Arc::clone(&index);
            handles.push(std::thread::spawn(move || {
                assert!(idx.cohort_of(OsStr::new("a")).is_some());
                assert!(idx.cohort_of(OsStr::new("b")).is_some());
            }));
        }
        for h in handles {
            h.join().expect("worker joined");
        }
    }

    fn flist_file(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::File::create(dir.join(name)).expect("create file");
    }

    #[test]
    fn compute_extras_generic_empty_dest_and_segment() {
        let dir = tempfile::TempDir::new().unwrap();
        let extras = compute_extras_generic(dir.path(), &[] as &[FileEntry]).unwrap();
        assert!(extras.is_empty());
    }

    #[test]
    fn compute_extras_generic_subtracts_segment_basenames() {
        let dir = tempfile::TempDir::new().unwrap();
        for n in ["a", "b", "c"] {
            touch(dir.path(), n);
        }
        let segment = vec![flist_file("a"), flist_file("c")];
        let extras = compute_extras_generic(dir.path(), &segment).unwrap();
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].name, OsString::from("b"));
    }

    #[test]
    fn compute_extras_generic_matches_concrete() {
        let dir = tempfile::TempDir::new().unwrap();
        for n in ["x", "y", "z"] {
            touch(dir.path(), n);
        }
        let segment = vec![flist_file("x")];
        let mut generic = compute_extras_generic(dir.path(), &segment).unwrap();
        let mut concrete = super::super::compute_extras(dir.path(), &segment).unwrap();
        generic.sort_by(|a, b| a.name.cmp(&b.name));
        concrete.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(generic, concrete);
    }

    #[test]
    fn compute_extras_with_cohorts_generic_tags_matching() {
        let dir = tempfile::TempDir::new().unwrap();
        for n in ["alpha", "beta", "untagged"] {
            touch(dir.path(), n);
        }
        let mut leader_entry = FileEntry::new_file(PathBuf::from("alpha"), 0, 0o644);
        leader_entry.set_hardlink_idx(u32::MAX);
        let mut member = FileEntry::new_file(PathBuf::from("beta"), 0, 0o644);
        member.set_hardlink_idx(0);
        let cohort_segment = vec![leader_entry, member];
        let index = super::super::CohortIndex::build_from_flist_segment(&cohort_segment);
        let extras =
            compute_extras_with_cohorts_generic(dir.path(), &[] as &[FileEntry], Some(&index))
                .unwrap();
        let by_name: std::collections::HashMap<_, _> = extras
            .iter()
            .map(|e| (e.name.clone(), e.hardlink_cohort))
            .collect();
        assert!(by_name[&OsString::from("alpha")].is_some());
        assert_eq!(
            by_name[&OsString::from("alpha")],
            by_name[&OsString::from("beta")]
        );
        assert!(by_name[&OsString::from("untagged")].is_none());
    }

    #[test]
    fn compute_extras_generic_nonexistent_dest_returns_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err = compute_extras_generic(&missing, &[] as &[FileEntry]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn compute_extras_with_cohorts_generic_none_matches_baseline() {
        let dir = tempfile::TempDir::new().unwrap();
        for n in ["a", "b", "c"] {
            touch(dir.path(), n);
        }
        let segment = vec![flist_file("a")];
        let mut generic = compute_extras_with_cohorts_generic(dir.path(), &segment, None).unwrap();
        let mut concrete =
            super::super::compute_extras_with_cohorts(dir.path(), &segment, None).unwrap();
        generic.sort_by(|a, b| a.name.cmp(&b.name));
        concrete.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(generic, concrete);
        for entry in &generic {
            assert!(entry.hardlink_cohort.is_none());
        }
    }

    fn dir_entry(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    fn file_entry(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    #[test]
    fn observe_segment_generic_filters_non_directories() {
        let mut cursor = super::super::DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment_generic(
            PathBuf::from("root"),
            &[
                dir_entry("root/sub"),
                file_entry("root/file.txt"),
                FileEntry::new_symlink(PathBuf::from("root/link"), PathBuf::from("target")),
            ],
        );
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(seq, vec![PathBuf::from("root"), PathBuf::from("root/sub")]);
    }

    #[test]
    fn observe_segment_generic_matches_concrete() {
        let children = vec![
            dir_entry("root/c"),
            dir_entry("root/a"),
            dir_entry("root/b"),
            file_entry("root/file.txt"),
        ];

        let mut generic_cursor = super::super::DirTraversalCursor::new(PathBuf::from("root"));
        generic_cursor.observe_segment_generic(PathBuf::from("root"), &children);
        let generic_seq: Vec<PathBuf> =
            std::iter::from_fn(|| generic_cursor.next_ready()).collect();

        let mut concrete_cursor = super::super::DirTraversalCursor::new(PathBuf::from("root"));
        concrete_cursor.observe_segment(PathBuf::from("root"), &children);
        let concrete_seq: Vec<PathBuf> =
            std::iter::from_fn(|| concrete_cursor.next_ready()).collect();

        assert_eq!(generic_seq, concrete_seq);
    }

    #[test]
    fn observe_segment_generic_deduplicates() {
        let mut cursor = super::super::DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment_generic(PathBuf::from("root"), &[dir_entry("root/a")]);
        cursor.observe_segment_generic(PathBuf::from("root"), &[dir_entry("root/a")]);
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(seq, vec![PathBuf::from("root"), PathBuf::from("root/a")]);
    }

    #[test]
    fn observe_segment_generic_depth_first_order() {
        let mut cursor = super::super::DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment_generic(
            PathBuf::from("root/a"),
            &[dir_entry("root/a/y"), dir_entry("root/a/x")],
        );
        cursor.observe_segment_generic(
            PathBuf::from("root"),
            &[dir_entry("root/b"), dir_entry("root/a")],
        );
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(
            seq,
            vec![
                PathBuf::from("root"),
                PathBuf::from("root/a"),
                PathBuf::from("root/a/x"),
                PathBuf::from("root/a/y"),
                PathBuf::from("root/b"),
            ]
        );
    }
}
