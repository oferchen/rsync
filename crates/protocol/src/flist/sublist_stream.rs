//! Incremental counterpart of [`SubListPlan`](super::SubListPlan).
//!
//! [`SubListPlan`](super::SubListPlan) partitions a complete, sorted file list
//! at once. A writer that streams its output, as upstream's sender does, learns
//! the list one directory at a time: it reads a directory, sorts its entries,
//! and only then knows that directory's sub-list. [`SubListPlanner`] accepts
//! those directory scans in any order and releases each list as soon as every
//! list before it in send order is known, with the same `ndx_start`,
//! `dir_ndx` and member order the one-shot plan computes.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_extra_file_list()` - sub-lists go out depth-first, each
//!   announced as `NDX_FLIST_OFFSET - dir_ndx`
//! - `flist.c:3503,3511` - each list's `ndx_start`
//! - `flist.c:recv_file_list()` - `dir_flist` grows in reception order

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{DirectoryTree, FileEntry};

/// One file list released by a [`SubListPlanner`], in send order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamedList {
    /// Wire `dir_ndx` of the owning directory; `None` for the initial list.
    pub parent_dir_ndx: Option<i32>,
    /// Caller id of the owning directory entry; `None` for the initial list.
    pub parent: Option<usize>,
    /// The NDX of the first member.
    pub ndx_start: i32,
    /// Caller ids of the members, in list order.
    pub members: Vec<usize>,
}

impl StreamedList {
    /// The gap NDX just below this list: where its owning directory (or `.`
    /// for the initial list) is itemized.
    ///
    /// upstream: generator.c:2780-2788, rsync.c:flist_for_ndx()
    #[must_use]
    pub const fn gap_ndx(&self) -> i32 {
        self.ndx_start - 1
    }
}

/// A scan the planner cannot place.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubListPlanError {
    /// An entry's parent directory is not a directory of an earlier list.
    ///
    /// Upstream cannot produce one (`send_implied_dirs()` emits every
    /// ancestor) and a conformant receiver aborts on it (`flist.c:3491-3506`).
    #[error("parent directory is missing from the file list: {0}")]
    Orphan(PathBuf),
    /// A directory was scanned twice.
    #[error("directory scanned twice: {0}")]
    DuplicateScan(PathBuf),
    /// The initial list was given twice.
    #[error("initial file list given twice")]
    DuplicateInitial,
}

/// Plans upstream's incremental-recursion lists from directory scans.
#[derive(Debug, Default)]
pub struct SubListPlanner {
    tree: DirectoryTree,
    /// Directory path -> (tree node, sub-list slot).
    dirs: HashMap<PathBuf, (usize, usize)>,
    /// Per slot: the owning directory's caller id and, once scanned, its members.
    slots: Vec<(usize, Option<Vec<usize>>)>,
    /// Caller id -> wire `dir_ndx`, for directories already released.
    dir_ndx: HashMap<usize, i32>,
    /// Caller ids of every directory member seen, `.` and duplicates included:
    /// each takes a `dir_ndx` when its list is released.
    directories: HashSet<usize>,
    next_dir_ndx: i32,
    next_start: i32,
    initial: Option<StreamedList>,
    initial_released: bool,
}

impl SubListPlanner {
    /// Creates a planner with no lists.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_start: 1,
            ..Self::default()
        }
    }

    /// Records the initial list: the top-level entries, sorted in upstream's
    /// `f_name_cmp` order, each with the caller's id.
    ///
    /// # Errors
    ///
    /// [`SubListPlanError::Orphan`] for an entry below the top level, and
    /// [`SubListPlanError::DuplicateInitial`] on a second call.
    pub fn push_initial<'a>(
        &mut self,
        entries: impl IntoIterator<Item = (usize, &'a FileEntry)>,
    ) -> Result<(), SubListPlanError> {
        if self.initial.is_some() {
            return Err(SubListPlanError::DuplicateInitial);
        }
        let mut members = Vec::new();
        for (id, entry) in entries {
            let parent = entry.dirname().as_ref();
            if !(parent.as_os_str().is_empty() || parent == Path::new(".")) {
                return Err(SubListPlanError::Orphan(entry.path().to_path_buf()));
            }
            self.link(id, entry, None);
            if entry.is_dir() {
                self.dir_ndx.insert(id, self.next_dir_ndx);
                self.next_dir_ndx += 1;
            }
            members.push(id);
        }
        // upstream: flist.c:3514 - the first list starts at 1 under inc_recurse.
        let list = StreamedList {
            parent_dir_ndx: None,
            parent: None,
            ndx_start: self.next_start,
            members,
        };
        self.next_start += list.members.len() as i32 + 1;
        self.initial = Some(list);
        Ok(())
    }

    /// Records the scan of directory `dir`: its direct children, sorted, each
    /// with the caller's id. The scan may arrive before or after the lists
    /// that precede it in send order.
    ///
    /// # Errors
    ///
    /// [`SubListPlanError::Orphan`] when `dir` is not a directory of a known
    /// list or a child is not directly below it, and
    /// [`SubListPlanError::DuplicateScan`] when `dir` was scanned already.
    pub fn push_scan<'a>(
        &mut self,
        dir: &Path,
        entries: impl IntoIterator<Item = (usize, &'a FileEntry)>,
    ) -> Result<(), SubListPlanError> {
        let Some(&(node, slot)) = self.dirs.get(dir) else {
            return Err(SubListPlanError::Orphan(dir.to_path_buf()));
        };
        if self.slots[slot].1.is_some() {
            return Err(SubListPlanError::DuplicateScan(dir.to_path_buf()));
        }
        let mut members = Vec::new();
        for (id, entry) in entries {
            if entry.dirname().as_ref() != dir {
                return Err(SubListPlanError::Orphan(entry.path().to_path_buf()));
            }
            self.link(id, entry, Some(node));
            members.push(id);
        }
        self.slots[slot].1 = Some(members);
        Ok(())
    }

    /// Adds `entry`'s directory node under `parent` so the depth-first walk
    /// reaches it. `.` and `FLAG_DUPLICATE` siblings own no sub-list.
    ///
    /// upstream: flist.c:2649-2655
    fn link(&mut self, id: usize, entry: &FileEntry, parent: Option<usize>) {
        if entry.is_dir() {
            self.directories.insert(id);
        }
        let name = entry.path().as_path();
        if entry.is_dir() && name != Path::new(".") && !entry.duplicate() {
            let node = self
                .tree
                .add_directory(self.slots.len(), name.to_path_buf(), parent);
            self.dirs
                .insert(name.to_path_buf(), (node, self.slots.len()));
            self.slots.push((id, None));
        }
    }

    /// Releases the next list in send order, if every list before it and its
    /// own scan are known.
    pub fn pop_ready(&mut self) -> Option<StreamedList> {
        if !self.initial_released {
            let initial = self.initial.take()?;
            self.initial_released = true;
            return Some(initial);
        }
        let (_, slot, _) = self.tree.peek()?;
        let members = self.slots[slot].1.take()?;
        let parent = self.slots[slot].0;
        self.tree.advance_cursor();
        let ndx_start = self.next_start;
        self.next_start += members.len() as i32 + 1;
        // upstream: flist.c:recv_file_list() - a list's directories join
        // dir_flist as the list arrives.
        for &m in &members {
            if self.directories.contains(&m) {
                self.dir_ndx.insert(m, self.next_dir_ndx);
                self.next_dir_ndx += 1;
            }
        }
        Some(StreamedList {
            parent_dir_ndx: Some(self.dir_ndx[&parent]),
            parent: Some(parent),
            ndx_start,
            members,
        })
    }

    /// Whether every list has been released: the initial list is out and the
    /// depth-first walk has no directory left.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.initial_released && self.tree.traversal_finished()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use proptest::prelude::*;

    use super::*;
    use crate::flist::{SubListLookahead, SubListPlan, sort_file_list};

    /// A tree from generated paths: every ancestor becomes a directory, and a
    /// name used both as a file and as a parent is a directory.
    fn tree_from(paths: &[(Vec<u8>, bool)]) -> Vec<FileEntry> {
        let mut kinds: BTreeMap<PathBuf, bool> = BTreeMap::new();
        kinds.insert(PathBuf::from("."), true);
        for (components, dir) in paths {
            let mut path = PathBuf::new();
            for (depth, &c) in components.iter().enumerate() {
                path.push(char::from(b'a' + c).to_string());
                let is_leaf = depth + 1 == components.len();
                let entry = kinds.entry(path.clone()).or_insert(false);
                *entry |= !is_leaf || *dir;
            }
        }
        let mut entries: Vec<FileEntry> = kinds
            .into_iter()
            .map(|(path, dir)| {
                if dir {
                    FileEntry::new_directory(path, 0o755)
                } else {
                    FileEntry::new_file(path, 1, 0o644)
                }
            })
            .collect();
        sort_file_list(&mut entries, false, false);
        entries
    }

    fn top_level(entries: &[FileEntry]) -> Vec<usize> {
        (0..entries.len())
            .filter(|&i| {
                let parent = entries[i].dirname();
                parent.as_os_str().is_empty() || parent.as_ref() == Path::new(".")
            })
            .collect()
    }

    fn children(entries: &[FileEntry], dir: &Path) -> Vec<usize> {
        (0..entries.len())
            .filter(|&i| entries[i].dirname().as_ref() == dir && entries[i].path() != dir)
            .collect()
    }

    /// Every directory scan, in the depth-first order the local walk enters
    /// directories: each directory's children are read when it is entered,
    /// and its subdirectories are entered in list order.
    fn walk_scans(entries: &[FileEntry]) -> Vec<(PathBuf, Vec<usize>)> {
        fn visit(entries: &[FileEntry], dir: &Path, out: &mut Vec<(PathBuf, Vec<usize>)>) {
            let kids = children(entries, dir);
            out.push((dir.to_path_buf(), kids.clone()));
            for k in kids {
                if entries[k].is_dir() {
                    visit(entries, entries[k].path(), out);
                }
            }
        }
        let mut out = Vec::new();
        for i in top_level(entries) {
            if entries[i].is_dir() && entries[i].path() != Path::new(".") {
                visit(entries, entries[i].path(), &mut out);
            }
        }
        out
    }

    /// Runs the planner, pushing scans in `order` and draining after each.
    fn stream(entries: &[FileEntry], scans: &[(PathBuf, Vec<usize>)]) -> Vec<StreamedList> {
        let mut planner = SubListPlanner::new();
        planner
            .push_initial(top_level(entries).into_iter().map(|i| (i, &entries[i])))
            .expect("initial list");
        let mut lists = Vec::new();
        while let Some(list) = planner.pop_ready() {
            lists.push(list);
        }
        for (dir, kids) in scans {
            planner
                .push_scan(dir, kids.iter().map(|&i| (i, &entries[i])))
                .expect("scan");
            while let Some(list) = planner.pop_ready() {
                lists.push(list);
            }
        }
        assert!(
            planner.finished(),
            "every list released after the last scan"
        );
        lists
    }

    /// The one-shot plan, in the same shape.
    fn planned(entries: &[FileEntry]) -> Vec<StreamedList> {
        let plan = SubListPlan::new(entries);
        let starts = plan.ndx_starts();
        std::iter::once(StreamedList {
            parent_dir_ndx: None,
            parent: None,
            ndx_start: starts[0],
            members: plan.initial.clone(),
        })
        .chain(
            plan.sublists
                .iter()
                .zip(&starts[1..])
                .map(|(s, &start)| StreamedList {
                    parent_dir_ndx: Some(s.parent_dir_ndx),
                    parent: Some(s.parent),
                    ndx_start: start,
                    members: s.members.clone(),
                }),
        )
        .collect()
    }

    /// The NDX every entry goes out under, derived from released lists the way
    /// `SubListPlan::wire_ndx` does.
    fn wire_ndx(entries: &[FileEntry], lists: &[StreamedList]) -> Vec<i32> {
        let mut ndx = vec![0i32; entries.len()];
        for list in lists {
            for (pos, &m) in list.members.iter().enumerate() {
                ndx[m] = list.ndx_start + pos as i32;
            }
        }
        if let Some(&dot) = lists[0]
            .members
            .first()
            .filter(|&&i| entries[i].name() == ".")
        {
            ndx[dot] = lists[0].gap_ndx();
        }
        for list in &lists[1..] {
            ndx[list.parent.expect("sub-list owner")] = list.gap_ndx();
        }
        ndx
    }

    /// A random order of `scans` that still scans each directory after the
    /// directory that lists it, the only orders a walk can produce: a
    /// directory is known only once its parent has been read.
    fn random_walk_order(
        scans: &[(PathBuf, Vec<usize>)],
        mut seed: u64,
    ) -> Vec<(PathBuf, Vec<usize>)> {
        let mut pending = scans.to_vec();
        let mut out: Vec<(PathBuf, Vec<usize>)> = Vec::with_capacity(scans.len());
        while !pending.is_empty() {
            let ready: Vec<usize> = (0..pending.len())
                .filter(|&i| {
                    let parent = pending[i].0.parent().unwrap_or(Path::new(""));
                    parent.as_os_str().is_empty() || out.iter().any(|(dir, _)| dir == parent)
                })
                .collect();
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let pick = ready[(seed >> 33) as usize % ready.len()];
            out.push(pending.remove(pick));
        }
        out
    }

    fn paths() -> impl Strategy<Value = Vec<(Vec<u8>, bool)>> {
        prop::collection::vec((prop::collection::vec(0u8..4, 1..=4), any::<bool>()), 0..40)
    }

    proptest! {
        /// Scans in the walk's own order release exactly the one-shot plan.
        #[test]
        fn walk_order_scans_release_the_one_shot_plan(paths in paths()) {
            let entries = tree_from(&paths);
            let lists = stream(&entries, &walk_scans(&entries));
            prop_assert_eq!(&lists, &planned(&entries));
            prop_assert_eq!(
                wire_ndx(&entries, &lists),
                SubListPlan::new(&entries).wire_ndx(&entries)
            );
        }

        /// Scans in any order a walk can produce (each directory after its
        /// parent) still release the lists in send order, unchanged.
        #[test]
        fn out_of_order_scans_release_the_same_lists(paths in paths(), seed in any::<u64>()) {
            let entries = tree_from(&paths);
            let scans = random_walk_order(&walk_scans(&entries), seed);
            prop_assert_eq!(stream(&entries, &scans), planned(&entries));
        }

        /// An open lookahead fed list sizes as they are planned makes the same
        /// sub-list decisions as the closed one, waiting only while a decision
        /// needs a size it has not been given.
        #[test]
        fn open_lookahead_matches_the_closed_schedule(paths in paths()) {
            let entries = tree_from(&paths);
            let plan = SubListPlan::new(&entries);
            let starts = plan.ndx_starts();
            let records: Vec<i32> = SubListPlan::new(&entries).wire_ndx(&entries);
            let mut records = records;
            records.sort_unstable();
            let want = plan.lookahead_schedule(&records);
            let sizes: Vec<usize> = plan.sublists.iter().map(|s| s.members.len()).collect();
            let mut open = SubListLookahead::open(plan.initial.len());
            let mut pushed = 0;
            for (k, &ndx) in records.iter().enumerate() {
                let list = starts.partition_point(|&start| start - 1 <= ndx).saturating_sub(1);
                let got = loop {
                    if let Some(sent) = open.try_serve(list) {
                        break sent;
                    }
                    if pushed < sizes.len() {
                        open.push_list(sizes[pushed]);
                        pushed += 1;
                    } else {
                        open.close();
                    }
                };
                prop_assert_eq!(got, want[k], "record {}", k);
            }
        }
    }

    #[test]
    fn a_scan_of_an_unknown_directory_is_an_orphan() {
        let entries = tree_from(&[(vec![0, 1], false)]);
        let mut planner = SubListPlanner::new();
        planner
            .push_initial(top_level(&entries).into_iter().map(|i| (i, &entries[i])))
            .expect("initial list");
        let err = planner.push_scan(Path::new("z"), std::iter::empty());
        assert_eq!(err, Err(SubListPlanError::Orphan(PathBuf::from("z"))));
    }

    #[test]
    fn a_directory_scanned_twice_is_refused() {
        let entries = tree_from(&[(vec![0, 1], false)]);
        let mut planner = SubListPlanner::new();
        planner
            .push_initial(top_level(&entries).into_iter().map(|i| (i, &entries[i])))
            .expect("initial list");
        let kids = children(&entries, Path::new("a"));
        planner
            .push_scan(Path::new("a"), kids.iter().map(|&i| (i, &entries[i])))
            .expect("first scan");
        let again = planner.push_scan(Path::new("a"), kids.iter().map(|&i| (i, &entries[i])));
        assert_eq!(
            again,
            Err(SubListPlanError::DuplicateScan(PathBuf::from("a")))
        );
    }

    /// The release waits for the next directory in send order: `b`'s scan
    /// alone releases nothing while `a`, which precedes it, is unscanned.
    #[test]
    fn a_later_scan_waits_for_the_earlier_directory() {
        let entries = tree_from(&[(vec![0, 2], false), (vec![1, 2], false)]);
        let mut planner = SubListPlanner::new();
        planner
            .push_initial(top_level(&entries).into_iter().map(|i| (i, &entries[i])))
            .expect("initial list");
        assert!(
            planner.pop_ready().is_some(),
            "the initial list is ready at once"
        );
        let b = children(&entries, Path::new("b"));
        planner
            .push_scan(Path::new("b"), b.iter().map(|&i| (i, &entries[i])))
            .expect("scan b");
        assert_eq!(planner.pop_ready(), None);
        let a = children(&entries, Path::new("a"));
        planner
            .push_scan(Path::new("a"), a.iter().map(|&i| (i, &entries[i])))
            .expect("scan a");
        assert_eq!(planner.pop_ready().and_then(|l| l.parent), Some(1));
        assert_eq!(planner.pop_ready().and_then(|l| l.parent), Some(3));
        assert!(planner.finished());
    }
}
