//! Read-only hardlink-cohort snapshot consumed by the delete pipeline.
//!
//! [`CohortIndex`] is the Option A artefact from
//! `docs/design/hardlink-delete-audit.md` section 7.1. It is built once per
//! INC_RECURSE segment from a frozen `&[FileEntry]` slice, wrapped in an
//! [`std::sync::Arc`], and shared by reference across all phase-1
//! [`super::compute_extras_with_cohorts`] workers and the single phase-2
//! [`super::DeleteEmitter`]. The index is immutable for its lifetime: the
//! "phase-1 workers are pure readers" invariant is encoded in the type
//! system rather than enforced by discipline.
//!
//! # What it tracks
//!
//! For each source-side hardlink cohort visible in the segment, the index
//! records:
//!
//! - a stable [`super::HardlinkCohortId`] (the leader's wire NDX);
//! - the source-side basename of every member, so the delete worker can
//!   tag a candidate extra by name without re-statting;
//! - the source-side `(dev, ino)` of every member that ships the
//!   protocol-29 hardlink_dev/ino fields, so a worker that already holds a
//!   destination `stat()` can confirm cohort membership;
//! - the count of source-side refs per cohort, used by
//!   [`CohortIndex::surviving_refs_in_cohort`] to answer "how many
//!   destination paths in this cohort will the upstream still expect".
//!
//! # Why no locks
//!
//! The audit identifies four races (R1-R4) that would appear if phase-1
//! workers shared a mutable hardlink table. By construction every method
//! on [`CohortIndex`] takes `&self` and the underlying maps are populated
//! exactly once at build time and never mutated afterwards. Cloning the
//! [`std::sync::Arc`] is the only operation workers need.
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/hlink.c:59-65`
//!   (`init_hard_links`): builds the dev/ino table before the delete
//!   sweep runs.
//! - `target/interop/upstream-src/rsync-3.4.1/hlink.c:186-208`
//!   (`match_hard_links`): freezes leader assignments before
//!   `do_delete_pass`.
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
//!   (`delete_item`): the dispatch is unconditional `do_unlink`; the
//!   kernel reconciles ref counts. Our [`CohortIndex`] therefore exists
//!   to power itemize tagging and cohort-aware diagnostics, not to skip
//!   the unlink syscall.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;

use protocol::flist::FileEntry;
use rustc_hash::FxHashMap;

use super::plan::HardlinkCohortId;

/// Read-only snapshot of the hardlink cohort layout for one INC_RECURSE
/// segment.
///
/// Built via [`CohortIndex::build_from_flist_segment`] at the boundary
/// between file-list reception and delete dispatch. Phase-1 workers and
/// the single emitter read the snapshot through [`std::sync::Arc`]
/// clones; no method on this type mutates the underlying maps.
#[derive(Debug, Default)]
pub struct CohortIndex {
    /// Basename -> cohort id. Populated for every source-side hardlinked
    /// member observed in the segment. Used by
    /// [`CohortIndex::cohort_of`].
    by_name: HashMap<OsString, HardlinkCohortId>,
    /// `(dev, ino)` -> cohort id. Populated only for entries that ship
    /// the protocol-29 hardlink_dev/ino fields. Used by callers that
    /// already hold a destination `stat()` and want to confirm cohort
    /// identity without re-walking the file list. Matches upstream's
    /// `dev_tbl` indexing.
    by_dev_ino: FxHashMap<(u64, u64), HardlinkCohortId>,
    /// Cohort id -> count of source-side refs in this segment. The
    /// emitter uses this through
    /// [`CohortIndex::surviving_refs_in_cohort`] to label the itemize
    /// line when only a subset of a cohort's destination paths is being
    /// removed.
    cohort_sizes: FxHashMap<HardlinkCohortId, u32>,
}

impl CohortIndex {
    /// Builds a fresh snapshot from one frozen flist segment slice.
    ///
    /// The slice must already be in its final form for the segment;
    /// callers typically invoke this once per INC_RECURSE segment after
    /// `match_hard_links` has assigned leader indices. See section 9 of
    /// `docs/design/hardlink-delete-audit.md` for the integration point.
    ///
    /// Returns an [`std::sync::Arc`] so the caller can hand the snapshot
    /// to rayon workers and to the emitter without an extra clone.
    ///
    /// # Cohort identity
    ///
    /// Each member of a cohort carries the leader's flist index in
    /// `FileEntry::hardlink_idx()`. The leader itself reports
    /// [`u32::MAX`] (upstream's "first occurrence" marker) and is
    /// identified by its position in the segment - the index of the
    /// leader entry within the segment is used as the cohort id so the
    /// id is stable across both leader and followers.
    #[must_use]
    pub fn build_from_flist_segment(entries: &[FileEntry]) -> Arc<Self> {
        let mut leader_for_idx: FxHashMap<u32, HardlinkCohortId> = FxHashMap::default();
        let mut by_name: HashMap<OsString, HardlinkCohortId> = HashMap::new();
        let mut by_dev_ino: FxHashMap<(u64, u64), HardlinkCohortId> = FxHashMap::default();
        let mut cohort_sizes: FxHashMap<HardlinkCohortId, u32> = FxHashMap::default();

        // First pass: identify leaders. A leader's `hardlink_idx()`
        // returns `Some(u32::MAX)` in the upstream encoding. We mint the
        // cohort id from the leader's position in the segment so that
        // followers carrying the same wire NDX can be matched against
        // it.
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

        // Second pass: attach every follower to its leader. Followers
        // carry the leader's flist index in `hardlink_idx()`.
        for entry in entries.iter() {
            let Some(leader_ndx) = entry.hardlink_idx() else {
                continue;
            };
            if leader_ndx == u32::MAX {
                // Already handled as a leader above.
                continue;
            }
            let Some(&cohort) = leader_for_idx.get(&leader_ndx) else {
                // Cross-segment follower (the leader lives in an earlier
                // segment). The audit's INC_RECURSE rebuild contract
                // covers this: each segment owns its own cohort space
                // and cross-segment cohorts surface in the segment that
                // carries the leader. Skip rather than mint a synthetic
                // id.
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
    ///
    /// Returns `Some` only when the segment carries a source-side
    /// hardlinked member with the same basename. This is the lookup the
    /// delete worker performs when stat'ing an extras candidate to
    /// decide whether the itemize line should carry a cohort tag.
    #[must_use]
    pub fn cohort_of(&self, name: &OsStr) -> Option<HardlinkCohortId> {
        self.by_name.get(name).copied()
    }

    /// Looks up the cohort id by destination `(dev, ino)`.
    ///
    /// Available only when the segment carried protocol-29 hardlink
    /// dev/ino fields. Returns `None` if the segment is post-protocol-30
    /// (which omits per-entry dev/ino) or if the destination inode does
    /// not match any source-side cohort.
    #[must_use]
    pub fn cohort_by_dev_ino(&self, dev: u64, ino: u64) -> Option<HardlinkCohortId> {
        self.by_dev_ino.get(&(dev, ino)).copied()
    }

    /// Returns the count of source-side refs the segment expects to keep
    /// for the cohort.
    ///
    /// Phase-2 uses this to decide whether the per-path itemize line
    /// should be tagged as "last ref" or as one of several. The count
    /// reflects source-side membership only - destination-side ref
    /// counts are the kernel's responsibility, mirroring upstream
    /// `delete.c:130-225` where every unlink is unconditional.
    #[must_use]
    pub fn surviving_refs_in_cohort(&self, cohort: HardlinkCohortId) -> u32 {
        self.cohort_sizes.get(&cohort).copied().unwrap_or(0)
    }

    /// Returns the total number of distinct cohorts in this snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cohort_sizes.len()
    }

    /// Returns `true` when no source-side cohorts were observed in this
    /// segment.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cohort_sizes.is_empty()
    }

    /// Inserts one cohort member into the indexes. Pulled out so the
    /// leader and follower passes share the same bookkeeping; private to
    /// keep the publish-once invariant.
    fn record(
        entry: &FileEntry,
        cohort: HardlinkCohortId,
        by_name: &mut HashMap<OsString, HardlinkCohortId>,
        by_dev_ino: &mut FxHashMap<(u64, u64), HardlinkCohortId>,
        cohort_sizes: &mut FxHashMap<HardlinkCohortId, u32>,
    ) {
        if let Some(name) = basename_of(entry.path()) {
            by_name.insert(name, cohort);
        }
        if let (Some(dev), Some(ino)) = (entry.hardlink_dev(), entry.hardlink_ino()) {
            // Casts to u64 are safe: upstream uses unsigned dev/ino on
            // the wire and the i64 type is the protocol-29 carrier.
            by_dev_ino.insert((dev as u64, ino as u64), cohort);
        }
        let count = cohort_sizes.entry(cohort).or_insert(0);
        *count = count.saturating_add(1);
    }
}

/// Extracts a leaf basename from a relative path, returning `None` for
/// the degenerate empty path. Matches the segment-basename rule already
/// used by [`super::compute_extras`].
fn basename_of(path: &Path) -> Option<OsString> {
    path.file_name().map(OsStr::to_os_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    fn empty_segment_yields_empty_index() {
        let index = CohortIndex::build_from_flist_segment(&[]);
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.cohort_of(OsStr::new("missing")).is_none());
        assert!(index.cohort_by_dev_ino(1, 2).is_none());
    }

    #[test]
    fn non_hardlinked_segment_yields_empty_index() {
        // A plain file with no hardlink_idx set must not register a
        // cohort; FileEntry::hardlink_idx() returns None in that case.
        let entries = vec![
            FileEntry::new_file(PathBuf::from("a"), 0, 0o644),
            FileEntry::new_file(PathBuf::from("b"), 0, 0o644),
        ];
        let index = CohortIndex::build_from_flist_segment(&entries);
        assert!(index.is_empty());
        assert!(index.cohort_of(OsStr::new("a")).is_none());
    }

    #[test]
    fn single_leader_registers_one_cohort_of_size_one() {
        let entries = vec![leader("solo")];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let cohort = index
            .cohort_of(OsStr::new("solo"))
            .expect("leader registers by name");
        assert_eq!(cohort, HardlinkCohortId::new(0));
        assert_eq!(index.surviving_refs_in_cohort(cohort), 1);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn leader_and_followers_share_cohort_id() {
        // Leader at position 0; followers point at NDX 0.
        let entries = vec![leader("leader"), follower("ref1", 0), follower("ref2", 0)];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let cohort = index
            .cohort_of(OsStr::new("leader"))
            .expect("leader present");
        assert_eq!(index.cohort_of(OsStr::new("ref1")), Some(cohort));
        assert_eq!(index.cohort_of(OsStr::new("ref2")), Some(cohort));
        assert_eq!(index.surviving_refs_in_cohort(cohort), 3);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn surviving_refs_counts_only_source_side_refs() {
        // The source has 2 refs in this cohort; destination ref counts
        // are explicitly out of scope (kernel handles them at unlink).
        let entries = vec![leader("a"), follower("b", 0)];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let cohort = index.cohort_of(OsStr::new("a")).unwrap();
        assert_eq!(index.surviving_refs_in_cohort(cohort), 2);
        // Unknown cohort id returns 0.
        assert_eq!(
            index.surviving_refs_in_cohort(HardlinkCohortId::new(999)),
            0,
        );
    }

    #[test]
    fn multiple_distinct_cohorts_are_isolated() {
        // Two leaders at positions 0 and 2, followers at 1 and 3.
        let entries = vec![
            leader("g1_leader"),
            follower("g1_member", 0),
            leader("g2_leader"),
            follower("g2_member", 2),
        ];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let g1 = index.cohort_of(OsStr::new("g1_leader")).unwrap();
        let g2 = index.cohort_of(OsStr::new("g2_leader")).unwrap();
        assert_ne!(g1, g2);
        assert_eq!(g1, HardlinkCohortId::new(0));
        assert_eq!(g2, HardlinkCohortId::new(2));
        assert_eq!(index.cohort_of(OsStr::new("g1_member")), Some(g1));
        assert_eq!(index.cohort_of(OsStr::new("g2_member")), Some(g2));
        assert_eq!(index.surviving_refs_in_cohort(g1), 2);
        assert_eq!(index.surviving_refs_in_cohort(g2), 2);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn cross_segment_follower_without_leader_is_skipped() {
        // A follower pointing at a leader index that does not exist in
        // this segment (e.g. cross-segment INC_RECURSE) must not mint a
        // ghost cohort. The audit's per-segment rebuild contract relies
        // on this.
        let entries = vec![follower("orphan", 999)];
        let index = CohortIndex::build_from_flist_segment(&entries);
        assert!(index.is_empty());
        assert!(index.cohort_of(OsStr::new("orphan")).is_none());
    }

    #[test]
    fn dev_ino_lookup_returns_cohort_when_protocol_29_fields_present() {
        let entries = vec![
            leader_with_dev_ino("a", 5, 100),
            follower_with_dev_ino("b", 0, 5, 100),
        ];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let cohort = index.cohort_by_dev_ino(5, 100).expect("dev/ino indexed");
        assert_eq!(index.cohort_of(OsStr::new("a")), Some(cohort));
        assert_eq!(index.cohort_of(OsStr::new("b")), Some(cohort));
    }

    #[test]
    fn dev_ino_lookup_returns_none_when_fields_absent() {
        // Protocol-30+ entries do not ship hardlink_dev/ino. The by-name
        // lookup still works.
        let entries = vec![leader("a"), follower("b", 0)];
        let index = CohortIndex::build_from_flist_segment(&entries);
        assert!(index.cohort_by_dev_ino(0, 0).is_none());
        assert!(index.cohort_of(OsStr::new("a")).is_some());
    }

    #[test]
    fn segment_with_basename_in_subdirectory_indexes_by_leaf() {
        // The set-subtraction logic in compute_extras matches on
        // basenames; the cohort index must agree so a tag-by-name
        // lookup against a flat dest listing finds the cohort.
        let mut entry = FileEntry::new_file(PathBuf::from("sub/dir/leader.txt"), 0, 0o644);
        entry.set_hardlink_idx(u32::MAX);
        let index = CohortIndex::build_from_flist_segment(&[entry]);
        assert!(index.cohort_of(OsStr::new("leader.txt")).is_some());
        assert!(index.cohort_of(OsStr::new("sub/dir/leader.txt")).is_none());
    }

    #[test]
    fn entry_with_empty_path_is_skipped() {
        // Defensive: a degenerate row with no basename must not panic
        // and must not pollute the by-name map with an empty key.
        let mut leader_entry = FileEntry::new_file(PathBuf::new(), 0, 0o644);
        leader_entry.set_hardlink_idx(u32::MAX);
        let index = CohortIndex::build_from_flist_segment(&[leader_entry]);
        // The cohort still exists (size tracking is by id), but no
        // lookup-by-name can hit it.
        assert_eq!(index.len(), 1);
        assert!(index.cohort_of(OsStr::new("")).is_none());
    }

    #[test]
    fn arc_snapshot_is_shareable_across_threads() {
        let entries = vec![leader("a"), follower("b", 0)];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let index = Arc::clone(&index);
            handles.push(std::thread::spawn(move || {
                assert!(index.cohort_of(OsStr::new("a")).is_some());
                assert!(index.cohort_of(OsStr::new("b")).is_some());
                let cohort = index.cohort_of(OsStr::new("a")).unwrap();
                assert_eq!(index.surviving_refs_in_cohort(cohort), 2);
            }));
        }
        for h in handles {
            h.join().expect("worker joined");
        }
    }

    #[test]
    fn snapshot_is_not_mutated_after_build() {
        // The publish-once invariant: every observable method takes
        // &self, so once the Arc is handed out the only mutation
        // possible is replacing the Arc wholesale (per-segment rebuild).
        let entries = vec![leader("a"), follower("b", 0)];
        let index = CohortIndex::build_from_flist_segment(&entries);
        let first_len = index.len();
        let _other = Arc::clone(&index);
        assert_eq!(index.len(), first_len);
    }
}
