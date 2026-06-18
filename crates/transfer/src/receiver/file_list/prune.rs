//! Receiver-side `--prune-empty-dirs` pass.
//!
//! Mirrors upstream `flist.c:flist_sort_and_clean()` lines 3121-3184 from
//! rsync-3.4.4: after the receiver has sorted its file list, directories
//! whose subtree contains no non-directory entries are cleared. The sender
//! ships every directory unconditionally; only the receiver runs this prune
//! pass.
//!
//! Upstream uses `F_DEPTH(file)` as both the directory's positive depth and,
//! transiently, as a negative back-pointer into the previous candidate (the
//! linked chain of dirs that might still be pruned). When a kept non-dir
//! entry (or an excluded directory whose exclusion is decided locally)
//! reprieves the chain, F_DEPTH is restored to a positive depth. When the
//! walk passes the candidate's depth without a reprieve, the candidate is
//! `clear_file()`'d - upstream zeroes the slot but keeps it in place so that
//! the wire NDX still indexes into the array.
//!
//! This Rust port keeps `FileEntry` immutable on the depth field by carrying
//! the same state in a parallel `marker[]` slice: positive values store the
//! original depth, negative values encode `-prev_i - 1` to point back to the
//! previous candidate. To preserve NDX correspondence with the sender, pruned
//! entries are cleared in place (name reset, mode zeroed) rather than removed
//! from `file_list`. Downstream receiver iteration already skips entries with
//! `is_file() == false && is_dir() == false`, so cleared entries are no-ops.

use std::path::{Path, PathBuf};

use filters::FilterChain;
use protocol::flist::FileEntry;

/// Walks the receiver's sorted `file_list` and clears directories whose
/// subtrees contain no kept non-directory entries, mirroring upstream
/// `flist.c:3121-3184`.
///
/// `filter_chain` is consulted via [`FilterChain::allows`] with `is_dir=true`
/// to mirror upstream's `is_excluded(name, NAME_IS_DIR, ALL_FILTERS)` call.
/// Upstream returns 1 when the dir is excluded; we mirror that by checking
/// `!filter_chain.allows(path, true)`.
///
/// Pruned entries are cleared in place (name reset, mode zeroed) rather than
/// removed so that flat-array indices continue to map directly to the
/// sender's wire NDX. Downstream receiver iteration filters by `is_dir()` /
/// `is_file()`, both of which return `false` for a mode-zero entry, so the
/// cleared slots are silently skipped during transfer.
///
/// Must be called after [`protocol::flist::sort_file_list`] - the algorithm
/// depends on parent-then-children ordering for each directory subtree.
pub(in crate::receiver) fn prune_empty_dirs_pass(
    file_list: &mut [FileEntry],
    filter_chain: &FilterChain,
) {
    if file_list.is_empty() {
        return;
    }

    let n = file_list.len();
    // marker[i] mirrors upstream's transient F_DEPTH value for entry i.
    // - For non-dir entries and dirs with depth 0 (root), marker stays at the
    //   original depth and is never inspected.
    // - For candidate dirs, marker[i] = -(prev_candidate_index + 1), forming a
    //   singly-linked chain reachable by `prev_i = (-marker[i] - 1) as usize`.
    // - When a candidate is reprieved, marker[i] is restored to its (new)
    //   positive depth.
    let mut marker: Vec<i64> = file_list.iter().map(|e| entry_depth(e) as i64).collect();
    // Parallel "cleared" flags - upstream's `clear_file()` zeroes the entry
    // in place; we batch the in-place clear into a final pass to keep the
    // walking algorithm focused on chain accounting.
    let mut cleared: Vec<bool> = vec![false; n];

    let mut prev_depth: i64 = 0;
    // upstream: flist.c:3124 - "It's OK that this isn't really true."
    let mut prev_i: usize = 0;

    for i in 0..n {
        let entry = &file_list[i];
        let is_dir = entry.is_dir();
        let depth = entry_depth(entry) as i64;

        if is_dir && depth > 0 {
            // upstream: flist.c:3133-3140 - "Dump empty dirs when coming back
            // down." Walk back through the candidate chain via prev_i,
            // clearing any candidates whose depth is at or below the current
            // dir's depth that were never reprieved.
            //
            // After clearing, upstream's `clear_file()` sets F_DEPTH to 1
            // (a positive value), which terminates any subsequent walk
            // through that index. We mirror that here so the next iteration
            // breaks if prev_i happened to chain back to itself (the case
            // where the first candidate dir is the only entry on the chain).
            for _ in (depth..=prev_depth).rev() {
                let m = marker[prev_i];
                if m >= 0 {
                    break;
                }
                let next_prev = (-m - 1) as usize;
                cleared[prev_i] = true;
                marker[prev_i] = 1;
                prev_i = next_prev;
            }

            prev_depth = depth;

            // upstream: flist.c:3142 - is_excluded(name, 1, ALL_FILTERS).
            // Returns 1 when the dir is excluded by the receiver's filter
            // chain. In that branch, upstream reprieves the chain.
            let dir_is_excluded = !filter_chain.allows(entry.path().as_path(), true);

            if dir_is_excluded {
                // upstream: flist.c:3143-3150 - "Keep dirs through this dir."
                // Walk the chain restoring F_DEPTH to descending depths.
                let mut j = prev_depth - 1;
                loop {
                    let m = marker[prev_i];
                    if m >= 0 {
                        break;
                    }
                    let next_prev = (-m - 1) as usize;
                    marker[prev_i] = j;
                    prev_i = next_prev;
                    j -= 1;
                }
            } else {
                // upstream: flist.c:3151-3152 - mark this dir as a candidate
                // whose F_DEPTH points back to the prior chain head.
                marker[i] = -(prev_i as i64) - 1;
            }

            prev_i = i;
        } else {
            // upstream: flist.c:3155-3162 - "Keep dirs through this non-dir."
            // Any non-dir (or depth-0 dir) entry proves its ancestor candidate
            // chain is non-empty; reprieve them all.
            let mut j = prev_depth;
            loop {
                let m = marker[prev_i];
                if m >= 0 {
                    break;
                }
                let next_prev = (-m - 1) as usize;
                marker[prev_i] = j;
                prev_i = next_prev;
                j -= 1;
            }
        }
    }

    // upstream: flist.c:3165-3172 - "Dump all remaining empty dirs."
    loop {
        let m = marker[prev_i];
        if m >= 0 {
            break;
        }
        let next_prev = (-m - 1) as usize;
        cleared[prev_i] = true;
        marker[prev_i] = 1;
        prev_i = next_prev;
    }

    // upstream: flist.c:3174-3183 retightens flist->low/high after clearing
    // entries. We do not maintain a low/high range; instead, mirror upstream's
    // `clear_file()` in place so flat indices keep mapping to wire NDX. The
    // downstream receiver iteration filters by `is_dir()` / `is_file()`, both
    // of which return `false` for `mode == 0`, so cleared slots are inert.
    for (i, was_cleared) in cleared.iter().enumerate() {
        if *was_cleared {
            file_list[i].set_name(PathBuf::new());
            file_list[i].set_mode(0);
        }
    }
}

/// Returns the upstream `F_DEPTH(file)` value for an entry.
///
/// upstream: flist.c:1103-1111 - depth is 1 + number of intermediate directory
/// separators in the relative path, decremented by 1 for dir entries whose
/// basename is `.`.
fn entry_depth(entry: &FileEntry) -> usize {
    let path: &Path = entry.path().as_path();
    let mut components: usize = 0;
    let mut basename_is_dot = false;
    for c in path.components() {
        if let std::path::Component::Normal(s) = c {
            components += 1;
            basename_is_dot = s == std::ffi::OsStr::new(".");
        } else {
            basename_is_dot = false;
        }
    }
    if components == 0 {
        return 0;
    }
    let depth = components;
    if entry.is_dir() && basename_is_dot {
        depth.saturating_sub(1)
    } else {
        depth
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use filters::FilterChain;
    use protocol::flist::FileEntry;
    use protocol::flist::sort_file_list;

    use super::prune_empty_dirs_pass;

    /// Builds a synthetic flist with a mix of empty directory chains and
    /// directories containing files, runs the prune pass, and verifies the
    /// empty chains are gone while populated subtrees survive.
    #[test]
    fn prunes_empty_chains_and_keeps_populated_subtrees() {
        let mut list = vec![
            FileEntry::new_directory(PathBuf::from("bar"), 0o755),
            FileEntry::new_directory(PathBuf::from("bar/down"), 0o755),
            FileEntry::new_directory(PathBuf::from("bar/down/to"), 0o755),
            FileEntry::new_directory(PathBuf::from("bar/down/to/bar"), 0o755),
            FileEntry::new_directory(PathBuf::from("bar/down/to/bar/baz"), 0o755),
            FileEntry::new_file(PathBuf::from("bar/down/to/bar/baz/keep.txt"), 4, 0o644),
            FileEntry::new_directory(PathBuf::from("bar/down/to/foo"), 0o755),
            FileEntry::new_directory(PathBuf::from("bar/down/to/foo/too"), 0o755),
            FileEntry::new_directory(PathBuf::from("foo"), 0o755),
            FileEntry::new_directory(PathBuf::from("foo/down"), 0o755),
            FileEntry::new_directory(PathBuf::from("foo/down/to"), 0o755),
            FileEntry::new_directory(PathBuf::from("foo/down/to/you"), 0o755),
            FileEntry::new_directory(PathBuf::from("foo/sub"), 0o755),
            FileEntry::new_file(PathBuf::from("foo/sub/file1"), 4, 0o644),
            FileEntry::new_directory(PathBuf::from("mid"), 0o755),
            FileEntry::new_directory(PathBuf::from("mid/for"), 0o755),
            FileEntry::new_directory(PathBuf::from("mid/for/foo"), 0o755),
            FileEntry::new_directory(PathBuf::from("mid/for/foo/and"), 0o755),
            FileEntry::new_directory(PathBuf::from("mid/for/foo/and/that"), 0o755),
            FileEntry::new_directory(PathBuf::from("mid/for/foo/and/that/is"), 0o755),
            FileEntry::new_directory(PathBuf::from("mid/for/foo/and/that/is/who"), 0o755),
            FileEntry::new_directory(PathBuf::from("new"), 0o755),
            FileEntry::new_directory(PathBuf::from("new/keep"), 0o755),
            FileEntry::new_directory(PathBuf::from("new/keep/this"), 0o755),
            FileEntry::new_directory(PathBuf::from("new/lose"), 0o755),
            FileEntry::new_directory(PathBuf::from("new/lose/this"), 0o755),
        ];

        sort_file_list(&mut list, false, false);
        prune_empty_dirs_pass(&mut list, &FilterChain::empty());

        // After prune, kept entries still have non-empty names; cleared
        // entries have been zeroed in place (`set_name(PathBuf::new())` +
        // `set_mode(0)`) so they are inert for downstream iteration.
        let kept: Vec<String> = list
            .iter()
            .filter(|e| !e.name().is_empty())
            .map(|e| e.name().to_string())
            .collect();
        // Populated subtree under bar/down/to/bar/baz survives.
        assert!(kept.iter().any(|n| n == "bar"));
        assert!(kept.iter().any(|n| n == "bar/down"));
        assert!(kept.iter().any(|n| n == "bar/down/to"));
        assert!(kept.iter().any(|n| n == "bar/down/to/bar"));
        assert!(kept.iter().any(|n| n == "bar/down/to/bar/baz"));
        assert!(kept.iter().any(|n| n == "bar/down/to/bar/baz/keep.txt"));
        assert!(kept.iter().any(|n| n == "foo"));
        assert!(kept.iter().any(|n| n == "foo/sub"));
        assert!(kept.iter().any(|n| n == "foo/sub/file1"));

        // Empty chains are pruned.
        assert!(
            !kept.iter().any(|n| n == "bar/down/to/foo"),
            "expected bar/down/to/foo to be pruned, kept={kept:?}"
        );
        assert!(
            !kept.iter().any(|n| n == "bar/down/to/foo/too"),
            "expected bar/down/to/foo/too to be pruned, kept={kept:?}"
        );
        assert!(
            !kept.iter().any(|n| n == "foo/down"),
            "expected foo/down to be pruned, kept={kept:?}"
        );
        assert!(
            !kept.iter().any(|n| n == "foo/down/to"),
            "expected foo/down/to to be pruned, kept={kept:?}"
        );
        assert!(
            !kept.iter().any(|n| n == "foo/down/to/you"),
            "expected foo/down/to/you to be pruned, kept={kept:?}"
        );
        for empty in [
            "mid",
            "mid/for",
            "mid/for/foo",
            "mid/for/foo/and",
            "mid/for/foo/and/that",
            "mid/for/foo/and/that/is",
            "mid/for/foo/and/that/is/who",
            "new",
            "new/keep",
            "new/keep/this",
            "new/lose",
            "new/lose/this",
        ] {
            assert!(
                !kept.iter().any(|n| n == empty),
                "expected {empty} to be pruned, kept={kept:?}"
            );
        }
    }

    /// Empty flist must be a no-op.
    #[test]
    fn empty_list_is_noop() {
        let mut list: Vec<FileEntry> = Vec::new();
        prune_empty_dirs_pass(&mut list, &FilterChain::empty());
        assert!(list.is_empty());
    }

    /// A flist of only files (no directories) must survive untouched.
    #[test]
    fn files_only_survive() {
        let mut list = vec![
            FileEntry::new_file(PathBuf::from("a.txt"), 1, 0o644),
            FileEntry::new_file(PathBuf::from("b.txt"), 1, 0o644),
        ];
        sort_file_list(&mut list, false, false);
        prune_empty_dirs_pass(&mut list, &FilterChain::empty());
        let kept: Vec<String> = list
            .iter()
            .filter(|e| !e.name().is_empty())
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(kept.len(), 2);
    }

    /// A standalone empty directory (no children at all) is pruned in place.
    #[test]
    fn standalone_empty_dir_pruned() {
        let mut list = vec![
            FileEntry::new_directory(PathBuf::from("empty"), 0o755),
            FileEntry::new_file(PathBuf::from("file.txt"), 1, 0o644),
        ];
        sort_file_list(&mut list, false, false);
        prune_empty_dirs_pass(&mut list, &FilterChain::empty());
        // List length is unchanged so that flat indices keep mapping to wire NDX.
        assert_eq!(list.len(), 2);
        let kept: Vec<String> = list
            .iter()
            .filter(|e| !e.name().is_empty())
            .map(|e| e.name().to_string())
            .collect();
        assert!(
            !kept.iter().any(|n| n == "empty"),
            "expected empty/ pruned, kept={kept:?}"
        );
        assert!(kept.iter().any(|n| n == "file.txt"));
        // The cleared slot has mode zero so downstream is_dir / is_file return false.
        assert!(
            list.iter()
                .any(|e| e.name().is_empty() && !e.is_dir() && !e.is_file())
        );
    }

    /// NDX correspondence test: after prune, file slots that survive remain
    /// at the same flat index they occupied before prune. This is what allows
    /// the receiver to keep sending the sender's wire NDX values verbatim.
    #[test]
    fn preserves_flat_index_to_wire_ndx_correspondence() {
        let mut list = vec![
            FileEntry::new_directory(PathBuf::from("empty"), 0o755),
            FileEntry::new_directory(PathBuf::from("keep"), 0o755),
            FileEntry::new_file(PathBuf::from("keep/file.txt"), 1, 0o644),
        ];
        sort_file_list(&mut list, false, false);
        // Record (idx, name) for the surviving file to compare across prune.
        let file_idx_before = list
            .iter()
            .position(|e| e.name() == "keep/file.txt")
            .expect("file must be present");
        prune_empty_dirs_pass(&mut list, &FilterChain::empty());
        let file_idx_after = list
            .iter()
            .position(|e| e.name() == "keep/file.txt")
            .expect("file must still be present");
        assert_eq!(
            file_idx_before, file_idx_after,
            "pruning empty siblings must not shift surviving file indices"
        );
    }
}
