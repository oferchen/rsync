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
/// # Sorting Rules (Protocol 29+)
///
/// 1. "." always sorts first (root directory marker)
/// 2. At each directory depth, non-directories (files, symlinks, etc.) sort BEFORE directories
/// 3. Directories are compared as if they have a trailing '/'
/// 4. Within the same type, sort by byte comparison
/// 5. Directory contents follow the directory entry
///
/// # Total Order Guarantee
///
/// This function implements a total order by using a canonical comparison
/// that is transitive: if a < b and b < c, then a < c.
#[must_use]
pub fn compare_file_entries(a: &FileEntry, b: &FileEntry) -> Ordering {
    // Use byte comparison directly - rsync protocol uses bytes, not UTF-8
    let bytes_a = a.name_bytes();
    let bytes_b = b.name_bytes();

    // "." always comes first
    match (bytes_a == b".", bytes_b == b".") {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }

    // For directories, conceptually append '/' for comparison purposes.
    // This matches upstream rsync's f_name_cmp() which treats directories
    // as having an implicit trailing slash.
    let a_is_dir = a.is_dir();
    let b_is_dir = b.is_dir();

    // Compare byte by byte, treating directory names as having implicit '/'
    let mut i = 0;
    loop {
        // Get effective byte at position i, with implicit '/' for directories at end
        let ch_a = if i < bytes_a.len() {
            bytes_a[i]
        } else if i == bytes_a.len() && a_is_dir {
            b'/' // Implicit trailing slash for directory
        } else {
            0 // Past end
        };

        let ch_b = if i < bytes_b.len() {
            bytes_b[i]
        } else if i == bytes_b.len() && b_is_dir {
            b'/' // Implicit trailing slash for directory
        } else {
            0 // Past end
        };

        // Check for end condition
        let a_done = i > bytes_a.len() || (i == bytes_a.len() && !a_is_dir);
        let b_done = i > bytes_b.len() || (i == bytes_b.len() && !b_is_dir);

        if a_done && b_done {
            return Ordering::Equal;
        }
        if a_done {
            // a ended, b continues
            // If b's next char is '/', a is parent of b
            if ch_b == b'/' {
                return Ordering::Less;
            }
            // Otherwise a sorts before b (0 < any char)
            return Ordering::Less;
        }
        if b_done {
            // b ended, a continues
            if ch_a == b'/' {
                return Ordering::Greater;
            }
            return Ordering::Greater;
        }

        if ch_a != ch_b {
            // At the divergence point, check if we're comparing at same depth
            // by looking for '/' in the remaining parts
            let remaining_a = &bytes_a[i..];
            let remaining_b = &bytes_b[i..];

            // Check if there's a separator before any difference in the current component
            let a_has_sep_next = remaining_a.iter().position(|&c| c == b'/');
            let b_has_sep_next = remaining_b.iter().position(|&c| c == b'/');

            // Determine if entries are files or directories at this level
            let a_is_dir_here = a_has_sep_next.is_some() || (a_is_dir && a_has_sep_next.is_none());
            let b_is_dir_here = b_has_sep_next.is_some() || (b_is_dir && b_has_sep_next.is_none());

            // At each level, files sort before directories
            match (a_is_dir_here, b_is_dir_here) {
                (true, false) => return Ordering::Greater, // a is dir, b is file -> b first
                (false, true) => return Ordering::Less,    // a is file, b is dir -> a first
                _ => {} // Same type, compare bytes
            }

            // Same type at this level - compare the effective bytes
            return ch_a.cmp(&ch_b);
        }

        i += 1;
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

        let mut names = Vec::with_capacity(entries.len());
        names.extend(entries.iter().map(|e| e.name()));
        assert_eq!(
            names,
            vec![".", "another.txt", "test.txt", "subdir", "subdir/file.txt"]
        );
    }

    #[test]
    fn sort_files_at_root_before_nested() {
        let mut entries = vec![make_file("z.txt"), make_file("a/nested.txt"), make_dir("a")];

        sort_file_list(&mut entries);

        let mut names = Vec::with_capacity(entries.len());
        names.extend(entries.iter().map(|e| e.name()));
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
        let mut names = Vec::with_capacity(cleaned.len());
        names.extend(cleaned.iter().map(|e| e.name()));
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
        let mut names = Vec::with_capacity(cleaned.len());
        names.extend(cleaned.iter().map(|e| e.name()));
        assert_eq!(names, vec!["a.txt", "z.txt", "a"]);
    }
}
