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
//! | `traversal::DirTraversalCursor::observe_segment` | [`collect_child_dirs_generic`] |
//! | `cohort_index::CohortIndex::build_from_flist_segment` | [`build_cohort_index_generic`] |
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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::flist::FileEntryAccessor;
use rustc_hash::FxHashMap;

use super::plan::HardlinkCohortId;

// ---------------------------------------------------------------------------
// segment_basenames_generic (mirrors extras::segment_basenames)
// ---------------------------------------------------------------------------

/// Collects the leaf basenames from a slice of accessor-backed entries into
/// a hash set keyed by [`OsString`].
///
/// Mirrors [`super::extras::segment_basenames`] but works with any
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

// ---------------------------------------------------------------------------
// collect_child_dirs_generic (mirrors traversal logic in observe_segment)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// build_cohort_index_generic (mirrors CohortIndex::build_from_flist_segment)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use protocol::flist::FileEntry;

    use super::*;

    // -----------------------------------------------------------------------
    // segment_basenames_generic tests
    // -----------------------------------------------------------------------

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
        let entries = vec![
            FileEntry::new_file("a/b/c/d/leaf.txt".into(), 0, 0o644),
        ];
        let set = segment_basenames_generic(&entries);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&OsString::from("leaf.txt")));
    }

    // -----------------------------------------------------------------------
    // collect_child_dirs_generic tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // GenericCohortIndex tests
    // -----------------------------------------------------------------------

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
        let entries = vec![
            leader("a"),
            follower("b", 0),
            leader("c"),
            follower("d", 2),
        ];
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
}
