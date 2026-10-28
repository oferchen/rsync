//! Partition of a sorted file list into upstream's incremental-recursion lists.
//!
//! Under `CF_INC_RECURSE` a sender emits the top-level entries as the initial
//! list, then one sub-list per directory, each announced on the wire as
//! `NDX_FLIST_OFFSET - dir_ndx`. `dir_ndx` indexes the receiver's `dir_flist`,
//! which grows in reception order: every directory of the initial list
//! (including `.`) in list order, then the directories of each sub-list as the
//! sub-lists arrive in depth-first order.
//!
//! [`SubListPlan`] computes that partition once, from a sorted entry slice,
//! for every writer that emits an incremental file list: the network sender
//! and the local batch writer.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_extra_file_list()` - one sub-list per directory, depth-first
//! - `flist.c:add_dirs_to_tree()` - the traversal tree the sub-lists follow
//! - `flist.c:recv_file_list()` - appends each list's directories to `dir_flist`

use std::collections::HashMap;
use std::path::Path;

use super::{DirectoryTree, FileEntry};

/// The lookahead a sender keeps: it sends further sub-lists while fewer than
/// this many entries are queued beyond the list it is working on.
///
/// upstream: rsync.h:151 `#define MIN_FILECNT_LOOKAHEAD 1000`, used by
/// sender.c:516,550 `send_extra_file_list(f_out, MIN_FILECNT_LOOKAHEAD)`
pub const MIN_FILECNT_LOOKAHEAD: usize = 1000;

/// One directory's sub-list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubList {
    /// Wire `dir_ndx` of the owning directory in the receiver's `dir_flist`.
    pub parent_dir_ndx: i32,
    /// Index of the owning directory entry in the planned slice.
    pub parent: usize,
    /// Indices of the directory's direct children, in slice order.
    pub members: Vec<usize>,
}

/// The incremental-recursion layout of a sorted file list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubListPlan {
    /// Indices of the top-level entries, in slice order.
    pub initial: Vec<usize>,
    /// Sub-lists in send (depth-first) order.
    pub sublists: Vec<SubList>,
    /// Entries whose parent directory is not in the slice.
    ///
    /// Upstream cannot produce one (`send_implied_dirs()` emits every ancestor),
    /// and a conformant receiver aborts on it (`flist.c:3491-3506`). They are
    /// placed in the initial list so the stream stays in step, and listed here
    /// so the caller can report them.
    pub orphans: Vec<usize>,
}

impl SubListPlan {
    /// Plans the incremental layout of `entries`, which must be sorted in
    /// upstream's `f_name_cmp` order.
    #[must_use]
    pub fn new(entries: &[FileEntry]) -> Self {
        let mut plan = Self::default();
        let mut tree = DirectoryTree::new();
        // Directory path -> (tree node, sub-list slot). Keyed by the raw path:
        // a lossy key would merge names that differ only in invalid bytes.
        let mut dirs: HashMap<&Path, (usize, usize)> = HashMap::new();
        // Sub-list slots indexed by tree node id; filled in creation order.
        let mut slots: Vec<(usize, Vec<usize>)> = Vec::new();

        for (i, entry) in entries.iter().enumerate() {
            let name = entry.path().as_path();
            let parent = entry.dirname().as_ref();
            if parent.as_os_str().is_empty() || parent == Path::new(".") {
                // upstream: flist.c:2638-2644 - a FLAG_DUPLICATE sibling keeps its
                // dir_flist slot but owns no sub-list; its children stay with the
                // surviving directory.
                if entry.is_dir() && name != Path::new(".") && !entry.duplicate() {
                    let node = tree.add_directory(slots.len(), name.to_path_buf(), None);
                    dirs.insert(name, (node, slots.len()));
                    slots.push((i, Vec::new()));
                }
                plan.initial.push(i);
            } else if let Some(&(parent_node, slot)) = dirs.get(parent) {
                if entry.is_dir() {
                    let node =
                        tree.add_directory(slots.len(), name.to_path_buf(), Some(parent_node));
                    dirs.insert(name, (node, slots.len()));
                    slots.push((i, Vec::new()));
                }
                slots[slot].1.push(i);
            } else {
                plan.orphans.push(i);
                plan.initial.push(i);
            }
        }

        // dir_ndx follows the receiver's dir_flist growth: every directory of
        // the initial list, `.` included, then each sub-list's directories as
        // the sub-lists arrive.
        let mut wire_ndx: HashMap<usize, i32> = HashMap::new();
        let mut next_ndx = 0i32;
        for &i in &plan.initial {
            if entries[i].is_dir() {
                wire_ndx.insert(i, next_ndx);
                next_ndx += 1;
            }
        }
        while let Some((slot, _)) = tree.next_directory() {
            let (parent, members) = std::mem::take(&mut slots[slot]);
            for &m in &members {
                if entries[m].is_dir() {
                    wire_ndx.insert(m, next_ndx);
                    next_ndx += 1;
                }
            }
            plan.sublists.push(SubList {
                parent_dir_ndx: wire_ndx[&parent],
                parent,
                members,
            });
        }
        plan
    }

    /// Number of file lists the layout emits: the initial list plus one per
    /// sub-list.
    #[must_use]
    pub fn list_count(&self) -> usize {
        1 + self.sublists.len()
    }

    /// The wire `ndx_start` of each list, initial list first.
    ///
    /// upstream: flist.c:3503 - the first list starts at 1 under
    /// `inc_recurse`; flist.c:3511 - each later one at
    /// `prev->ndx_start + prev->used + 1`.
    #[must_use]
    pub fn ndx_starts(&self) -> Vec<i32> {
        let mut starts = Vec::with_capacity(self.list_count());
        let mut start = 1i32;
        let mut used = self.initial.len() as i32;
        starts.push(start);
        for sublist in &self.sublists {
            start += used + 1;
            starts.push(start);
            used = sublist.members.len() as i32;
        }
        starts
    }

    /// The number of sub-lists a sender has sent before each file-data
    /// record, for records given in send order by NDX.
    ///
    /// Before handling each request the sender sends sub-lists while fewer
    /// than [`MIN_FILECNT_LOOKAHEAD`] entries are queued beyond the list the
    /// request belongs to; a request for a list not yet sent waits until it is.
    /// Sub-lists left over after the last record go out after it.
    ///
    /// upstream: sender.c:515-547 (`send_extra_file_list()` around each
    /// `read_ndx_and_attrs()`), flist.c:2651 (`file_total - file_old_total`),
    /// rsync.c:396-401 (`file_old_total` recounted through the request's list)
    #[must_use]
    pub fn lookahead_schedule(&self, record_ndxs: &[i32]) -> Vec<usize> {
        let starts = self.ndx_starts();
        let used: Vec<usize> = std::iter::once(self.initial.len())
            .chain(self.sublists.iter().map(|s| s.members.len()))
            .collect();
        let mut file_total = used[0];
        let mut file_old_total = used[0];
        let mut sent = 0usize;
        let mut current = 0usize;
        let mut schedule = Vec::with_capacity(record_ndxs.len());
        for &ndx in record_ndxs {
            // A list owns the gap NDX just below its start (rsync.c flist_for_ndx()).
            let list = starts
                .partition_point(|&start| start - 1 <= ndx)
                .saturating_sub(1);
            if list != current {
                current = list;
                file_old_total = used[..=list].iter().sum();
            }
            while sent < self.sublists.len()
                && (sent < list || file_total < file_old_total + MIN_FILECNT_LOOKAHEAD)
            {
                sent += 1;
                file_total += used[sent];
            }
            schedule.push(sent);
        }
        schedule
    }

    /// The NDX each planned entry goes out under, indexed like `entries`.
    ///
    /// A file's NDX is its list's `ndx_start` plus its position in the list.
    /// A directory that owns a sub-list is itemized under the gap NDX just
    /// below that list (`ndx_start - 1`), and `.` under the initial list's:
    /// at its own NDX the generator only pre-creates it.
    ///
    /// upstream: generator.c:1630-1633 (`is_dir < 0` below the gap),
    /// generator.c:2780-2788 (the gap itemize), rsync.c:flist_for_ndx() (the
    /// receiver maps a gap NDX back to the owning directory).
    #[must_use]
    pub fn wire_ndx(&self, entries: &[FileEntry]) -> Vec<i32> {
        let starts = self.ndx_starts();
        let mut ndx = vec![0i32; entries.len()];
        let lists = std::iter::once(&self.initial).chain(self.sublists.iter().map(|s| &s.members));
        for (members, &start) in lists.zip(&starts) {
            for (pos, &i) in members.iter().enumerate() {
                ndx[i] = start + pos as i32;
            }
        }
        if let Some(&dot) = self.initial.first().filter(|&&i| entries[i].name() == ".") {
            ndx[dot] = starts[0] - 1;
        }
        for (sublist, &start) in self.sublists.iter().zip(&starts[1..]) {
            ndx[sublist.parent] = start - 1;
        }
        ndx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flist::sort_file_list;

    fn tree(paths: &[(&str, bool)]) -> Vec<FileEntry> {
        let mut entries: Vec<FileEntry> = paths
            .iter()
            .map(|&(p, dir)| {
                if dir {
                    FileEntry::new_directory(p.into(), 0o755)
                } else {
                    FileEntry::new_file(p.into(), 1, 0o644)
                }
            })
            .collect();
        sort_file_list(&mut entries, false, false);
        entries
    }

    fn names(entries: &[FileEntry], idx: &[usize]) -> Vec<String> {
        idx.iter()
            .map(|&i| entries[i].path().display().to_string())
            .collect()
    }

    /// The layout rsync 3.5.1 recorded for this tree with `-r --write-batch`:
    /// the top level, then `e` (dir 1, empty), `sub` (dir 2) and `sub/deep`
    /// (dir 3), with `.` as dir 0.
    #[test]
    fn plans_upstreams_dir_ndx_order() {
        let entries = tree(&[
            (".", true),
            ("a", false),
            ("e", true),
            ("sub", true),
            ("sub/b", false),
            ("sub/deep", true),
            ("sub/deep/c", false),
        ]);
        let plan = SubListPlan::new(&entries);
        assert_eq!(names(&entries, &plan.initial), [".", "a", "e", "sub"]);
        let got: Vec<(i32, Vec<String>)> = plan
            .sublists
            .iter()
            .map(|s| (s.parent_dir_ndx, names(&entries, &s.members)))
            .collect();
        assert_eq!(
            got,
            [
                (1, vec![]),
                (2, vec!["sub/b".to_owned(), "sub/deep".to_owned()]),
                (3, vec!["sub/deep/c".to_owned()]),
            ]
        );
        assert!(plan.orphans.is_empty());
    }

    /// flist.c:3503, 3511 - 4 initial entries at 1..=4, then the empty `e`
    /// list at 6, `sub` at 7, `sub/deep` at 10.
    #[test]
    fn ndx_starts_leave_one_gap_per_list() {
        let entries = tree(&[
            (".", true),
            ("a", false),
            ("e", true),
            ("sub", true),
            ("sub/b", false),
            ("sub/deep", true),
            ("sub/deep/c", false),
        ]);
        assert_eq!(SubListPlan::new(&entries).ndx_starts(), [1, 6, 7, 10]);
    }

    /// The schedule 3.5.1 followed recording 12 directories of 150 files: the
    /// first 7 sub-lists (1050 entries) up front, then one more each time the
    /// records move into the next list, until none are left.
    #[test]
    fn lookahead_follows_upstreams_schedule() {
        let mut paths = vec![(".".to_owned(), true)];
        for d in 1..=12 {
            paths.push((format!("d{d:02}"), true));
            for f in 1..=150 {
                paths.push((format!("d{d:02}/f{f:03}"), false));
            }
        }
        let refs: Vec<(&str, bool)> = paths.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let entries = tree(&refs);
        let plan = SubListPlan::new(&entries);
        let starts = plan.ndx_starts();
        let mut records = Vec::new();
        let mut want = Vec::new();
        for (k, sublist) in plan.sublists.iter().enumerate() {
            let start = starts[k + 1];
            records.push(start - 1);
            records.extend((0..sublist.members.len() as i32).map(|i| start + i));
            want.extend(std::iter::repeat_n(
                (8 + k).min(12),
                sublist.members.len() + 1,
            ));
        }
        assert_eq!(plan.lookahead_schedule(&records), want);
    }

    /// A request for a list the lookahead has not reached yet still finds that
    /// list on the stream ahead of it.
    #[test]
    fn a_record_never_precedes_its_list() {
        let entries = tree(&[
            (".", true),
            ("a", true),
            ("a/x", false),
            ("b", true),
            ("b/y", false),
        ]);
        let plan = SubListPlan::new(&entries);
        let starts = plan.ndx_starts();
        assert_eq!(plan.lookahead_schedule(&[starts[2]]), [2]);
    }

    /// The NDXs 3.5.1 used for this tree: `.` itemized at 0 and each directory
    /// at the gap below its own list (e 5, sub 6, sub/deep 9), never at its
    /// slot in the parent's list.
    #[test]
    fn directories_go_out_under_their_lists_gap_ndx() {
        let entries = tree(&[
            (".", true),
            ("a", false),
            ("e", true),
            ("sub", true),
            ("sub/b", false),
            ("sub/deep", true),
            ("sub/deep/c", false),
        ]);
        let plan = SubListPlan::new(&entries);
        let got: Vec<(String, i32)> = plan
            .wire_ndx(&entries)
            .into_iter()
            .enumerate()
            .map(|(i, n)| (entries[i].path().display().to_string(), n))
            .collect();
        let want = [
            (".", 0),
            ("a", 2),
            ("e", 5),
            ("sub", 6),
            ("sub/b", 7),
            ("sub/deep", 9),
            ("sub/deep/c", 10),
        ];
        assert_eq!(got, want.map(|(p, n)| (p.to_owned(), n)).to_vec());
    }

    #[test]
    fn an_entry_without_its_parent_is_an_orphan() {
        let entries = tree(&[(".", true), ("lost/x", false)]);
        let plan = SubListPlan::new(&entries);
        assert_eq!(names(&entries, &plan.orphans), ["lost/x"]);
        assert_eq!(names(&entries, &plan.initial), [".", "lost/x"]);
    }
}
