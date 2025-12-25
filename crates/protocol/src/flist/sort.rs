//! crates/protocol/src/flist/sort.rs
//!
//! File list sorting for rsync protocol.
//!
//! Both sender and receiver must sort their file lists identically after
//! building/receiving them. This ensures NDX (file index) values match
//! on both sides.
//!
//! # Upstream Reference
//!
//! - `flist.c:flist_sort_and_clean()` - Sorts file list after build/receive
//! - `flist.c:f_name_cmp()` - File entry comparison function
//!
//! The sorting algorithm follows these rules:
//! 1. "." (root directory marker) always comes first
//! 2. Files sort before directories at the same level
//! 3. Within each category (files or directories), sort alphabetically
//! 4. Directory contents immediately follow the directory entry

use std::cmp::Ordering;

use super::FileEntry;

/// Compares two file entries according to rsync's sorting rules.
///
/// This mirrors upstream's `f_name_cmp()` from `flist.c`.
///
/// # Sorting Rules
///
/// 1. "." always sorts first (root directory marker)
/// 2. At each directory level, files sort before directories
/// 3. Within files or directories at the same level, sort alphabetically
/// 4. A directory is immediately followed by its contents
///
/// # Example
///
/// For entries: `test.txt`, `subdir/`, `subdir/file.txt`, `another.txt`
///
/// Sorted order:
/// - `.` (root marker, if present)
/// - `another.txt` (file at root, 'a' < 's' < 't')
/// - `test.txt` (file at root)
/// - `subdir/` (directory at root, after files)
/// - `subdir/file.txt` (contents of subdir)
#[must_use]
pub fn compare_file_entries(a: &FileEntry, b: &FileEntry) -> Ordering {
    let name_a = a.name();
    let name_b = b.name();

    // "." always comes first
    if name_a == "." {
        return Ordering::Less;
    }
    if name_b == "." {
        return Ordering::Greater;
    }

    let is_dir_a = a.is_dir();
    let is_dir_b = b.is_dir();

    // Get parent paths (empty string for root level)
    let parent_a = name_a.rfind('/').map_or("", |i| &name_a[..i]);
    let parent_b = name_b.rfind('/').map_or("", |i| &name_b[..i]);

    // Get just the filename component
    let file_a = name_a.rfind('/').map_or(name_a, |i| &name_a[i + 1..]);
    let file_b = name_b.rfind('/').map_or(name_b, |i| &name_b[i + 1..]);

    if parent_a == parent_b {
        // Same parent directory - at the same level
        // Files sort before directories, then alphabetically within each group
        match (is_dir_a, is_dir_b) {
            (false, true) => Ordering::Less,    // file < dir
            (true, false) => Ordering::Greater, // dir > file
            _ => file_a.cmp(file_b),            // same type: alphabetical
        }
    } else if name_b.starts_with(name_a) && name_b.as_bytes().get(name_a.len()) == Some(&b'/') {
        // a is an ancestor of b (e.g., "subdir" vs "subdir/file.txt")
        // Ancestor comes first
        Ordering::Less
    } else if name_a.starts_with(name_b) && name_a.as_bytes().get(name_b.len()) == Some(&b'/') {
        // b is an ancestor of a
        Ordering::Greater
    } else {
        // Different parent directories
        // Compare the root-level components first
        let root_a = name_a.find('/').map_or(name_a, |i| &name_a[..i]);
        let root_b = name_b.find('/').map_or(name_b, |i| &name_b[..i]);

        if root_a == root_b {
            // Same root, different subpaths
            name_a.cmp(name_b)
        } else {
            // Different roots - check if they're files or dirs at root
            let a_is_file_at_root = !is_dir_a && !name_a.contains('/');
            let b_is_file_at_root = !is_dir_b && !name_b.contains('/');
            let a_is_under_dir = name_a.contains('/');
            let b_is_under_dir = name_b.contains('/');

            match (a_is_file_at_root, b_is_file_at_root) {
                (true, false) if b_is_under_dir || is_dir_b => Ordering::Less,
                (false, true) if a_is_under_dir || is_dir_a => Ordering::Greater,
                _ => root_a.cmp(root_b),
            }
        }
    }
}

/// Sorts a file list in-place according to rsync's sorting rules.
///
/// Both sender and receiver must call this after building/receiving
/// the file list to ensure matching NDX indices.
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` - Called after `send_file_list()`
///   and `recv_file_list()` to sort entries.
pub fn sort_file_list(file_list: &mut [FileEntry]) {
    file_list.sort_by(compare_file_entries);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(name: &str) -> FileEntry {
        FileEntry::new_file(name.into(), 0, 0o644)
    }

    fn make_dir(name: &str) -> FileEntry {
        FileEntry::new_directory(name.into(), 0o755)
    }

    #[test]
    fn dot_always_first() {
        let dot = make_dir(".");
        let file = make_file("abc.txt");
        assert_eq!(compare_file_entries(&dot, &file), Ordering::Less);
        assert_eq!(compare_file_entries(&file, &dot), Ordering::Greater);
    }

    #[test]
    fn files_before_dirs_at_same_level() {
        let file = make_file("zebra.txt");
        let dir = make_dir("aardvark");
        // Even though 'a' < 'z' alphabetically, files come before dirs
        assert_eq!(compare_file_entries(&file, &dir), Ordering::Less);
    }

    #[test]
    fn alphabetical_within_files() {
        let a = make_file("a.txt");
        let b = make_file("b.txt");
        assert_eq!(compare_file_entries(&a, &b), Ordering::Less);
    }

    #[test]
    fn alphabetical_within_dirs() {
        let a = make_dir("adir");
        let b = make_dir("bdir");
        assert_eq!(compare_file_entries(&a, &b), Ordering::Less);
    }

    #[test]
    fn dir_contents_follow_dir() {
        let dir = make_dir("subdir");
        let file_in_dir = make_file("subdir/file.txt");
        assert_eq!(compare_file_entries(&dir, &file_in_dir), Ordering::Less);
    }

    #[test]
    fn nested_dir_contents() {
        let parent = make_dir("a");
        let child = make_dir("a/b");
        let grandchild = make_file("a/b/c.txt");
        assert_eq!(compare_file_entries(&parent, &child), Ordering::Less);
        assert_eq!(compare_file_entries(&child, &grandchild), Ordering::Less);
    }

    #[test]
    fn sort_mixed_entries() {
        let mut entries = vec![
            make_file("test.txt"),
            make_dir("subdir"),
            make_file("subdir/file.txt"),
            make_file("another.txt"),
            make_dir("."),
        ];

        sort_file_list(&mut entries);

        let names: Vec<_> = entries.iter().map(|e| e.name()).collect();
        assert_eq!(
            names,
            vec![".", "another.txt", "test.txt", "subdir", "subdir/file.txt"]
        );
    }

    #[test]
    fn sort_files_at_root_before_nested() {
        let mut entries = vec![make_file("z.txt"), make_file("a/nested.txt"), make_dir("a")];

        sort_file_list(&mut entries);

        let names: Vec<_> = entries.iter().map(|e| e.name()).collect();
        // z.txt is a file at root, so it comes before the 'a' directory
        assert_eq!(names, vec!["z.txt", "a", "a/nested.txt"]);
    }
}
