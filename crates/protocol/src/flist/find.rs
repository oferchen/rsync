//! Name lookup in a sorted file list.
//!
//! upstream: flist.c:3383-3470 - `flist_find()`, `flist_find_name()` and
//! `flist_find_ignore_dirness()` binary-search `flist->sorted` with the same
//! `f_name_cmp()` order the list was sorted in.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::path::Path;

use super::FileEntry;
use super::sort::compare_entry_with_name;
use super::wire_path::path_bytes_to_wire;

/// Finds the entry named `name` in a list sorted by [`super::sort_file_list`],
/// looking for a non-directory first and then a directory.
///
/// Cleared (deduplicated) slots are stepped over. The probe is compared as raw
/// bytes, so on Unix the lookup allocates nothing.
///
/// upstream: flist.c:3434 `flist_find_name(flist, name, -1)`, which is
/// `flist_find_ignore_dirness()` (flist.c:3463) over `flist_find()`.
#[must_use]
pub fn flist_find_name<'a, E: Borrow<FileEntry>>(
    sorted: &'a [E],
    name: &Path,
    protocol_pre29: bool,
) -> Option<&'a FileEntry> {
    let name = path_bytes_to_wire(name);
    find_sorted(sorted, &name, false, protocol_pre29)
        .or_else(|| find_sorted(sorted, &name, true, protocol_pre29))
}

/// upstream: flist.c:3383 `flist_find()` - binary search that steps over
/// cleared slots. Below protocol 29 it also returns -1 on a dirness mismatch,
/// but `flist_find_ignore_dirness()` then retries with the other kind and
/// finds the same entry, so the outcome needs no check of its own.
fn find_sorted<'a, E: Borrow<FileEntry>>(
    sorted: &'a [E],
    name: &[u8],
    name_is_dir: bool,
    pre29: bool,
) -> Option<&'a FileEntry> {
    let (mut low, mut high) = (0usize, sorted.len());
    while low < high {
        let mid = low + (high - low) / 2;
        let Some(probe) = (mid..high).find(|&i| sorted[i].borrow().is_active()) else {
            high = mid;
            continue;
        };
        let found = sorted[probe].borrow();
        match compare_entry_with_name(found, name, name_is_dir, pre29) {
            Ordering::Equal => return Some(found),
            Ordering::Less => low = probe + 1,
            Ordering::Greater => high = mid,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::flist::sort_file_list;

    fn dir(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    fn file(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 1, 0o644)
    }

    fn sorted(mut list: Vec<FileEntry>, pre29: bool) -> Vec<FileEntry> {
        sort_file_list(&mut list, false, pre29);
        list
    }

    /// WHY: the lookup must find a directory wherever the sort put it, at both
    /// orders, or a valid list would be refused as an invalid path.
    #[test]
    fn finds_every_directory_in_a_sorted_list() {
        for pre29 in [false, true] {
            let list = sorted(
                vec![
                    dir("."),
                    file("a1"),
                    dir("d"),
                    file("d/f"),
                    dir("d/e"),
                    file("d-"),
                    file("d/e/g"),
                    dir("zz"),
                    file("zz/q"),
                ],
                pre29,
            );
            for name in ["d", "d/e", "zz"] {
                let found = flist_find_name(&list, Path::new(name), pre29);
                assert!(
                    found.is_some_and(FileEntry::is_dir),
                    "{name} not found at pre29={pre29}"
                );
            }
            for missing in ["dM", "d/x", "e", "zz/q/r"] {
                assert!(flist_find_name(&list, Path::new(missing), pre29).is_none());
            }
        }
    }

    /// WHY: a cleared duplicate keeps its slot with an empty name; a search
    /// that compared against it would lose its place in the order.
    #[test]
    fn steps_over_cleared_slots() {
        let mut list = sorted(
            vec![dir("."), dir("a"), dir("b"), dir("c"), dir("d")],
            false,
        );
        list[2].tombstone();
        list[3].tombstone();
        assert!(flist_find_name(&list, Path::new("d"), false).is_some());
        assert!(flist_find_name(&list, Path::new("a"), false).is_some());
        assert!(flist_find_name(&list, Path::new("c"), false).is_none());
    }

    /// WHY: upstream finds a same-named non-directory and only then rejects it
    /// as a parent (`S_ISDIR(f->mode)`), so the lookup must return it.
    #[test]
    fn a_non_directory_of_that_name_is_found() {
        let list = sorted(vec![dir("."), file("x"), file("y")], false);
        let found = flist_find_name(&list, Path::new("x"), false);
        assert!(found.is_some_and(|f| !f.is_dir()));
    }

    /// WHY: below protocol 29 `flist_find()` returns -1 on a dirness mismatch,
    /// and `flist_find_ignore_dirness()` then retries with the other kind.
    #[test]
    fn pre29_matches_either_kind_through_the_retry() {
        let list = sorted(vec![dir("x"), file("y")], true);
        assert!(flist_find_name(&list, Path::new("x"), true).is_some_and(FileEntry::is_dir));
        assert!(flist_find_name(&list, Path::new("y"), true).is_some_and(|f| !f.is_dir()));
    }
}
