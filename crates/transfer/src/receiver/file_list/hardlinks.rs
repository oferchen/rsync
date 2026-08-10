//! Post-sort hardlink leader/follower reconciliation.
//!
//! After the receiver sorts a file-list segment to match the sender's wire
//! order, the readdir-time `XMIT_HLINK_FIRST` flag set by the sender can no
//! longer be trusted - sorting may have moved a different entry to the
//! front of each hardlink group. These helpers re-derive the per-group
//! leader from sorted positions and, for legacy protocol < 30 streams,
//! synthesise group indices from raw `(dev, ino)` pairs so the rest of the
//! receiver can treat both wire variants uniformly.

use std::collections::HashMap;

use protocol::flist::FileEntry;

/// Reassigns hardlink leader/follower flags based on sorted order.
///
/// The sender sets `XMIT_HLINK_FIRST` during `send_file_entry()` based on readdir
/// (scan) order - the first file encountered with a given (dev, ino) pair gets the
/// flag. After sorting, the first file in sorted order may differ from the readdir
/// leader, especially when `--relative` introduces deep path components that change
/// the sort order.
///
/// This function mirrors upstream `hlink.c:match_gnums()`: it groups entries by
/// `hardlink_idx` (gnum) and assigns `XMIT_HLINK_FIRST` to the first entry in each
/// group in sorted (positional) order, clearing it on all others.
///
/// The `prior_hlinks` map persists across INC_RECURSE segments so that a follower
/// whose leader was received in a previous segment is correctly identified as a
/// follower rather than being promoted to leader (which would happen if only the
/// current segment's entries were considered).
///
/// Must be called after `sort_file_list()` and before any code that inspects
/// `hlink_first()` to decide leader vs follower (transfer, quick-check, hardlink
/// creation).
///
/// # Upstream Reference
///
/// - `hlink.c:match_gnums()` - post-sort leader/follower assignment with
///   `prior_hlinks` hashtable for cross-segment state
/// - `hlink.c:idev_find()` - two-level (dev, ino) hashtable lookup
pub(in crate::receiver) fn match_hard_links(
    entries: &mut [FileEntry],
    prior_hlinks: &mut HashMap<u32, bool>,
) {
    // Collect the first sorted position for each hardlink group within this segment.
    // Key: hardlink_idx (gnum), Value: index into entries slice.
    let mut first_in_group: HashMap<u32, usize> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        if let Some(idx) = entry.hardlink_idx() {
            first_in_group.entry(idx).or_insert(i);
        }
    }

    // Reassign flags: the leader is the first entry in sorted order, but only
    // if this gnum was not already seen in a prior INC_RECURSE segment.
    // upstream: hlink.c:match_gnums() - `node->data == data_when_new` check
    for (i, entry) in entries.iter_mut().enumerate() {
        if let Some(gnum) = entry.hardlink_idx() {
            let is_first_in_segment = first_in_group.get(&gnum) == Some(&i);
            let seen_before = prior_hlinks.contains_key(&gnum);

            if is_first_in_segment && !seen_before {
                // First occurrence of this gnum across all segments - this is the leader.
                entry.set_hlink_first(true);
                prior_hlinks.insert(gnum, true);
            } else {
                // Either not first in segment, or gnum was seen in a prior segment.
                entry.set_hlink_first(false);
                // Record the gnum even if it was already present, so future
                // segments know about it.
                prior_hlinks.entry(gnum).or_insert(true);
            }
        }
    }
}

/// Normalizes protocol 28-29 hardlink entries to use `hardlink_idx` and
/// `hlink_first` flags, matching the protocol 30+ representation.
///
/// For protocol < 30, the sender transmits raw (dev, ino) pairs instead of
/// hardlink group indices. This function groups entries by (dev, ino),
/// assigns a synthetic `hardlink_idx` to each group member, and sets
/// `hlink_first` on the first entry in sorted order. Entries with only one
/// occurrence of a (dev, ino) pair are left untouched - they are not part of
/// a hardlink group (nlink == 1 on the source).
///
/// After this normalization, `is_hardlink_follower()` and `create_hardlinks()`
/// work identically for both protocol versions.
///
/// # Upstream Reference
///
/// - `hlink.c:init_hard_links()` - builds hardlink table from (dev, ino) pairs
/// - `hlink.c:match_hard_links()` - assigns leader/follower after sorting
pub(in crate::receiver) fn normalize_pre30_hardlinks(entries: &mut [FileEntry]) {
    // Group entries by (dev, ino) pairs. Key: (dev, ino), Value: list of indices.
    let mut groups: HashMap<(i64, i64), Vec<usize>> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        if !entry.is_file() {
            continue;
        }
        let dev = match entry.hardlink_dev() {
            Some(d) => d,
            None => continue,
        };
        let ino = match entry.hardlink_ino() {
            Some(n) => n,
            None => continue,
        };
        groups.entry((dev, ino)).or_default().push(i);
    }

    // Assign synthetic hardlink_idx and hlink_first flags.
    // Only process groups with 2+ members (actual hardlinks).
    // Use the first entry's position as the group key to avoid collisions
    // with protocol 30+ gnum values.
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        // Use the first entry's sorted position as the group's gnum.
        let gnum = indices[0] as u32;
        for (pos, &idx) in indices.iter().enumerate() {
            entries[idx].set_hardlink_idx(gnum);
            entries[idx].set_hlinked(true);
            entries[idx].set_hlink_first(pos == 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::flist::FileEntry;

    /// Verifies that `match_hard_links` correctly assigns leader/follower flags
    /// when directory entries are interspersed among hardlinked files, as happens
    /// with `--relative` implied directories.
    ///
    /// After sorting, the first entry in each hardlink group (by sorted position)
    /// must get `hlink_first = true`. Directory entries that are not part of any
    /// hardlink group must be unaffected.
    #[test]
    fn match_hard_links_with_interspersed_directories() {
        // Simulate a sorted file list with implied directories from --relative:
        //   [0] dir  "a/"           (no hardlink)
        //   [1] file "a/orig.txt"   (group 7 - should become leader)
        //   [2] dir  "b/"           (no hardlink)
        //   [3] file "b/link.txt"   (group 7 - should become follower)
        let dir_a = FileEntry::new_directory("a".into(), 0o755);

        let mut leader = FileEntry::new_file("a/orig.txt".into(), 256, 0o644);
        leader.set_hardlink_idx(7);

        let dir_b = FileEntry::new_directory("b".into(), 0o755);

        let mut follower = FileEntry::new_file("b/link.txt".into(), 256, 0o644);
        follower.set_hardlink_idx(7);

        let mut entries = vec![dir_a, leader, dir_b, follower];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

        // Directory entries remain unaffected
        assert!(!entries[0].hlink_first());
        assert!(!entries[2].hlink_first());

        // First file in group 7 (sorted position 1) becomes leader
        assert!(entries[1].hlink_first());
        // Second file in group 7 (sorted position 3) becomes follower
        assert!(!entries[3].hlink_first());
    }

    /// Verifies `match_hard_links` handles multiple hardlink groups interspersed
    /// with directories. Each group independently assigns its own leader.
    #[test]
    fn match_hard_links_multiple_groups_with_directories() {
        //   [0] dir  "d/"             (no hardlink)
        //   [1] file "d/a.txt"        (group 1 - leader)
        //   [2] file "d/a_link.txt"   (group 1 - follower)
        //   [3] dir  "e/"             (no hardlink)
        //   [4] file "e/b.txt"        (group 4 - leader)
        //   [5] file "e/b_link.txt"   (group 4 - follower)
        let dir_d = FileEntry::new_directory("d".into(), 0o755);

        let mut la = FileEntry::new_file("d/a.txt".into(), 100, 0o644);
        la.set_hardlink_idx(1);

        let mut fa = FileEntry::new_file("d/a_link.txt".into(), 100, 0o644);
        fa.set_hardlink_idx(1);

        let dir_e = FileEntry::new_directory("e".into(), 0o755);

        let mut lb = FileEntry::new_file("e/b.txt".into(), 200, 0o644);
        lb.set_hardlink_idx(4);

        let mut fb = FileEntry::new_file("e/b_link.txt".into(), 200, 0o644);
        fb.set_hardlink_idx(4);

        let mut entries = vec![dir_d, la, fa, dir_e, lb, fb];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

        // Group 1: position 1 is leader, position 2 is follower
        assert!(entries[1].hlink_first());
        assert!(!entries[2].hlink_first());

        // Group 4: position 4 is leader, position 5 is follower
        assert!(entries[4].hlink_first());
        assert!(!entries[5].hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` assigns synthetic hardlink_idx and
    /// hlink_first from (dev, ino) pairs for a simple two-file group.
    #[test]
    fn normalize_pre30_two_file_group() {
        let mut a = FileEntry::new_file("a.txt".into(), 100, 0o644);
        a.set_hardlink_dev(1);
        a.set_hardlink_ino(42);

        let mut b = FileEntry::new_file("b.txt".into(), 100, 0o644);
        b.set_hardlink_dev(1);
        b.set_hardlink_ino(42);

        let mut entries = vec![a, b];
        normalize_pre30_hardlinks(&mut entries);

        // Both entries get the same hardlink_idx
        assert_eq!(entries[0].hardlink_idx(), entries[1].hardlink_idx());
        // First entry is leader
        assert!(entries[0].hlinked());
        assert!(entries[0].hlink_first());
        // Second entry is follower
        assert!(entries[1].hlinked());
        assert!(!entries[1].hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` leaves single-entry (dev, ino) pairs
    /// untouched - they are not part of a hardlink group.
    #[test]
    fn normalize_pre30_single_entry_not_grouped() {
        let mut a = FileEntry::new_file("only.txt".into(), 50, 0o644);
        a.set_hardlink_dev(99);
        a.set_hardlink_ino(1);

        let mut entries = vec![a];
        normalize_pre30_hardlinks(&mut entries);

        assert!(entries[0].hardlink_idx().is_none());
        assert!(!entries[0].hlinked());
        assert!(!entries[0].hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` handles multiple independent groups.
    #[test]
    fn normalize_pre30_multiple_groups() {
        let mut a1 = FileEntry::new_file("a1.txt".into(), 100, 0o644);
        a1.set_hardlink_dev(1);
        a1.set_hardlink_ino(10);

        let mut a2 = FileEntry::new_file("a2.txt".into(), 100, 0o644);
        a2.set_hardlink_dev(1);
        a2.set_hardlink_ino(10);

        let mut b1 = FileEntry::new_file("b1.txt".into(), 200, 0o644);
        b1.set_hardlink_dev(2);
        b1.set_hardlink_ino(20);

        let mut b2 = FileEntry::new_file("b2.txt".into(), 200, 0o644);
        b2.set_hardlink_dev(2);
        b2.set_hardlink_ino(20);

        let mut entries = vec![a1, a2, b1, b2];
        normalize_pre30_hardlinks(&mut entries);

        // Group A: entries 0, 1
        let idx_a = entries[0].hardlink_idx().unwrap();
        assert_eq!(entries[1].hardlink_idx().unwrap(), idx_a);
        assert!(entries[0].hlink_first());
        assert!(!entries[1].hlink_first());

        // Group B: entries 2, 3
        let idx_b = entries[2].hardlink_idx().unwrap();
        assert_eq!(entries[3].hardlink_idx().unwrap(), idx_b);
        assert!(entries[2].hlink_first());
        assert!(!entries[3].hlink_first());

        // Different groups have different indices
        assert_ne!(idx_a, idx_b);
    }

    /// Verifies `normalize_pre30_hardlinks` skips directories (only files are hardlinked).
    #[test]
    fn normalize_pre30_skips_directories() {
        let dir = FileEntry::new_directory("dir".into(), 0o755);

        let mut f1 = FileEntry::new_file("f1.txt".into(), 100, 0o644);
        f1.set_hardlink_dev(1);
        f1.set_hardlink_ino(5);

        let mut f2 = FileEntry::new_file("f2.txt".into(), 100, 0o644);
        f2.set_hardlink_dev(1);
        f2.set_hardlink_ino(5);

        let mut entries = vec![dir, f1, f2];
        normalize_pre30_hardlinks(&mut entries);

        // Directory is untouched
        assert!(entries[0].hardlink_idx().is_none());
        // Files are grouped
        assert_eq!(entries[1].hardlink_idx(), entries[2].hardlink_idx());
        assert!(entries[1].hlink_first());
        assert!(!entries[2].hlink_first());
    }

    /// Verifies `normalize_pre30_hardlinks` skips entries without dev/ino.
    #[test]
    fn normalize_pre30_skips_entries_without_dev_ino() {
        let plain = FileEntry::new_file("plain.txt".into(), 100, 0o644);

        let mut linked = FileEntry::new_file("linked.txt".into(), 100, 0o644);
        linked.set_hardlink_dev(1);
        linked.set_hardlink_ino(5);

        let mut entries = vec![plain, linked];
        normalize_pre30_hardlinks(&mut entries);

        // Neither entry forms a group of 2+, so no normalization
        assert!(entries[0].hardlink_idx().is_none());
        assert!(entries[1].hardlink_idx().is_none());
    }

    /// Verifies that `match_hard_links` reassigns the leader when the readdir-order
    /// leader appears after a follower in sorted order. This happens when --relative
    /// paths cause the sender's first-seen file to sort after another file in the
    /// same hardlink group.
    #[test]
    fn match_hard_links_reassigns_leader_after_sort() {
        // Sender saw "z/file.txt" first (readdir order), but after sort
        // "a/file.txt" comes first. Both share hardlink group 5.
        let mut entry_a = FileEntry::new_file("a/file.txt".into(), 300, 0o644);
        entry_a.set_hardlink_idx(5);
        // Sender marked this as follower (not first in readdir order)
        entry_a.set_hlink_first(false);

        let mut entry_z = FileEntry::new_file("z/file.txt".into(), 300, 0o644);
        entry_z.set_hardlink_idx(5);
        // Sender marked this as leader (first in readdir order)
        entry_z.set_hlink_first(true);

        let mut entries = vec![entry_a, entry_z];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut entries, &mut prior_hlinks);

        // After match_hard_links, sorted position 0 becomes the new leader
        assert!(
            entries[0].hlink_first(),
            "first in sorted order must be leader"
        );
        assert!(
            !entries[1].hlink_first(),
            "second in sorted order must be follower"
        );
    }

    /// Verifies that cross-segment hardlink followers are not promoted to leaders.
    ///
    /// When INC_RECURSE delivers entries in multiple segments, a follower whose
    /// leader was in a previous segment must remain a follower. Before the fix,
    /// `match_hard_links` only saw the current segment and would incorrectly
    /// promote such followers to leaders.
    ///
    /// upstream: hlink.c:match_gnums() - `prior_hlinks` hashtable persists across
    /// segments so cross-segment followers are correctly identified.
    #[test]
    fn cross_segment_follower_not_promoted_to_leader() {
        // Segment 1: contains the leader for gnum 42
        let mut leader = FileEntry::new_file("a/original.txt".into(), 512, 0o644);
        leader.set_hardlink_idx(42);

        let mut seg1 = vec![leader];
        let mut prior_hlinks = HashMap::new();
        match_hard_links(&mut seg1, &mut prior_hlinks);

        // Leader in segment 1 should be marked as leader
        assert!(
            seg1[0].hlink_first(),
            "first occurrence of gnum 42 must be leader"
        );
        // prior_hlinks should now contain gnum 42
        assert!(prior_hlinks.contains_key(&42));

        // Segment 2: contains a follower for gnum 42 (cross-directory hardlink)
        let mut follower = FileEntry::new_file("b/link.txt".into(), 512, 0o644);
        follower.set_hardlink_idx(42);

        let mut seg2 = vec![follower];
        match_hard_links(&mut seg2, &mut prior_hlinks);

        // Follower in segment 2 must NOT be promoted to leader - its leader is
        // in segment 1.
        assert!(
            !seg2[0].hlink_first(),
            "cross-segment follower must not be promoted to leader"
        );
    }

    /// Verifies that multiple segments with mixed gnums correctly identify
    /// leaders and followers across segment boundaries.
    #[test]
    fn cross_segment_mixed_groups() {
        let mut prior_hlinks = HashMap::new();

        // Segment 1: leader for gnum 10, follower for gnum 10, leader for gnum 20
        let mut a = FileEntry::new_file("a/file1.txt".into(), 100, 0o644);
        a.set_hardlink_idx(10);
        let mut b = FileEntry::new_file("a/file2.txt".into(), 100, 0o644);
        b.set_hardlink_idx(10);
        let mut c = FileEntry::new_file("a/file3.txt".into(), 200, 0o644);
        c.set_hardlink_idx(20);

        let mut seg1 = vec![a, b, c];
        match_hard_links(&mut seg1, &mut prior_hlinks);

        assert!(seg1[0].hlink_first(), "gnum 10 leader in seg1");
        assert!(!seg1[1].hlink_first(), "gnum 10 follower in seg1");
        assert!(seg1[2].hlink_first(), "gnum 20 leader in seg1");

        // Segment 2: follower for gnum 10, follower for gnum 20, leader for gnum 30
        let mut d = FileEntry::new_file("b/link1.txt".into(), 100, 0o644);
        d.set_hardlink_idx(10);
        let mut e = FileEntry::new_file("b/link3.txt".into(), 200, 0o644);
        e.set_hardlink_idx(20);
        let mut f = FileEntry::new_file("b/new.txt".into(), 300, 0o644);
        f.set_hardlink_idx(30);

        let mut seg2 = vec![d, e, f];
        match_hard_links(&mut seg2, &mut prior_hlinks);

        assert!(!seg2[0].hlink_first(), "gnum 10 cross-segment follower");
        assert!(!seg2[1].hlink_first(), "gnum 20 cross-segment follower");
        assert!(seg2[2].hlink_first(), "gnum 30 new leader in seg2");
    }
}
