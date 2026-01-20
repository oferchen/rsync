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

/// Result of cleaning a file list.
#[derive(Debug, Clone, Default)]
pub struct CleanResult {
    /// Number of duplicate entries removed.
    pub duplicates_removed: usize,
    /// Number of directory flags merged.
    pub flags_merged: usize,
}

/// Cleans a sorted file list by removing duplicates and merging directory flags.
///
/// This mirrors upstream's duplicate removal logic from `flist_sort_and_clean()`.
///
/// # Duplicate Handling Rules
///
/// When duplicate paths are found:
/// 1. If one is a directory and the other isn't, keep the directory
///    (it may have contents in the list)
/// 2. If both are directories, keep the first and merge flags
/// 3. Otherwise, keep the first entry
///
/// # Arguments
///
/// * `file_list` - A sorted file list (call `sort_file_list` first)
///
/// # Returns
///
/// A tuple of `(cleaned_list, CleanResult)` where `cleaned_list` contains
/// deduplicated entries and `CleanResult` has statistics.
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` lines 2979-3069
#[must_use]
pub fn flist_clean(file_list: Vec<FileEntry>) -> (Vec<FileEntry>, CleanResult) {
    if file_list.is_empty() {
        return (file_list, CleanResult::default());
    }

    let mut result = Vec::with_capacity(file_list.len());
    let mut stats = CleanResult::default();
    let mut iter = file_list.into_iter().peekable();

    while let Some(mut current) = iter.next() {
        // Check if next entry is a duplicate
        while let Some(next) = iter.peek() {
            if current.name() != next.name() {
                break;
            }

            // Duplicate found - decide which to keep
            let current_is_dir = current.is_dir();
            let next_is_dir = next.is_dir();

            match (current_is_dir, next_is_dir) {
                (false, true) => {
                    // Keep the directory (next), drop current
                    stats.duplicates_removed += 1;
                    current = iter.next().expect("peeked entry should exist");
                }
                (true, false) => {
                    // Keep current (directory), drop next
                    stats.duplicates_removed += 1;
                    let _ = iter.next();
                }
                (true, true) => {
                    // Both are directories - merge flags and keep current
                    // Upstream merges FLAG_TOP_DIR, FLAG_CONTENT_DIR flags
                    let next_entry = iter.next().expect("peeked entry should exist");
                    if next_entry.content_dir() {
                        current.set_content_dir(true);
                    }
                    stats.duplicates_removed += 1;
                    stats.flags_merged += 1;
                }
                (false, false) => {
                    // Both are files - keep first (current)
                    stats.duplicates_removed += 1;
                    let _ = iter.next();
                }
            }
        }

        result.push(current);
    }

    (result, stats)
}

/// Sorts and cleans a file list in one operation.
///
/// Combines `sort_file_list` and `flist_clean` for convenience.
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` - The combined operation
#[must_use]
pub fn sort_and_clean_file_list(mut file_list: Vec<FileEntry>) -> (Vec<FileEntry>, CleanResult) {
    file_list.sort_by(compare_file_entries);
    flist_clean(file_list)
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

    // flist_clean tests

    #[test]
    fn flist_clean_empty_list() {
        let entries: Vec<FileEntry> = vec![];
        let (cleaned, stats) = flist_clean(entries);
        assert!(cleaned.is_empty());
        assert_eq!(stats.duplicates_removed, 0);
        assert_eq!(stats.flags_merged, 0);
    }

    #[test]
    fn flist_clean_no_duplicates() {
        let entries = vec![make_file("a.txt"), make_file("b.txt"), make_dir("c")];
        let (cleaned, stats) = flist_clean(entries);
        assert_eq!(cleaned.len(), 3);
        assert_eq!(stats.duplicates_removed, 0);
    }

    #[test]
    fn flist_clean_removes_file_duplicates() {
        // Two files with same name - keep first
        let entries = vec![make_file("a.txt"), make_file("a.txt"), make_file("b.txt")];
        let (cleaned, stats) = flist_clean(entries);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(stats.duplicates_removed, 1);
        let names: Vec<_> = cleaned.iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn flist_clean_keeps_dir_over_file() {
        // Directory vs file with same name - keep directory
        let entries = vec![make_file("item"), make_dir("item")];
        let (cleaned, stats) = flist_clean(entries);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(stats.duplicates_removed, 1);
        assert!(cleaned[0].is_dir());
    }

    #[test]
    fn flist_clean_keeps_dir_over_file_reverse_order() {
        // Directory first, then file with same name - still keep directory
        let entries = vec![make_dir("item"), make_file("item")];
        let (cleaned, stats) = flist_clean(entries);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(stats.duplicates_removed, 1);
        assert!(cleaned[0].is_dir());
    }

    #[test]
    fn flist_clean_merges_directory_flags() {
        // Two directories with same name - merge flags, keep first
        let mut dir1 = make_dir("subdir");
        dir1.set_content_dir(false);
        let dir2 = make_dir("subdir"); // content_dir is true by default
        let entries = vec![dir1, dir2];
        let (cleaned, stats) = flist_clean(entries);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(stats.flags_merged, 1);
        // Flag should be merged (content_dir should be true since dir2 had it)
        assert!(cleaned[0].content_dir());
    }

    #[test]
    fn flist_clean_multiple_duplicates() {
        let entries = vec![
            make_file("a.txt"),
            make_file("a.txt"),
            make_file("a.txt"),
            make_file("b.txt"),
        ];
        let (cleaned, stats) = flist_clean(entries);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(stats.duplicates_removed, 2);
    }

    #[test]
    fn sort_and_clean_combined() {
        let entries = vec![
            make_file("z.txt"),
            make_dir("a"),
            make_file("a.txt"),
            make_file("a.txt"), // duplicate
        ];
        let (cleaned, stats) = sort_and_clean_file_list(entries);
        assert_eq!(stats.duplicates_removed, 1);
        let names: Vec<_> = cleaned.iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["a.txt", "z.txt", "a"]);
    }
}
