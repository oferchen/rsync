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

use logging::debug_log;
use memchr::memrchr;

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
///
/// # Performance
///
/// At the divergence point, the comparator must determine whether each entry
/// still has deeper path components (i.e., a '/' exists at or after position `i`).
/// Instead of scanning forward from `i` on every divergence (O(remaining_length)),
/// we precompute the last '/' position via `memrchr` once per call and answer
/// the query in O(1): `last_slash >= i` means a separator remains.
/// This matches upstream's approach of avoiding forward scans in `f_name_cmp()`
/// (upstream: flist.c:3217).
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

    // Precompute last '/' position for O(1) "has separator remaining?" queries.
    // memrchr uses SIMD on supported platforms, making this a single fast pass.
    let last_slash_a = memrchr(b'/', bytes_a);
    let last_slash_b = memrchr(b'/', bytes_b);

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
            return Ordering::Less;
        }
        if b_done {
            return Ordering::Greater;
        }

        if ch_a != ch_b {
            // At the divergence point, determine if each entry has deeper path
            // components remaining. A '/' exists at or after position `i` iff the
            // precomputed last_slash position is >= i.
            let a_has_sep = last_slash_a.is_some_and(|pos| pos >= i);
            let b_has_sep = last_slash_b.is_some_and(|pos| pos >= i);

            // Entry is "directory-like at this level" if it has more path components
            // (separator found ahead) or if it's a directory entry in its final component
            let a_is_dir_here = a_has_sep || a_is_dir;
            let b_is_dir_here = b_has_sep || b_is_dir;

            // At each level, files sort before directories
            match (a_is_dir_here, b_is_dir_here) {
                (true, false) => return Ordering::Greater, // a is dir, b is file -> b first
                (false, true) => return Ordering::Less,    // a is file, b is dir -> a first
                _ => {}                                    // Same type, compare bytes
            }

            // Same type at this level - compare the effective bytes
            return ch_a.cmp(&ch_b);
        }

        i += 1;
    }
}

/// Sorts a file list in-place according to rsync's sorting rules.
///
/// Uses index-based sorting to minimize memory traffic: only 8-byte
/// indices are shuffled during comparisons, then a single permutation
/// pass moves the ~160-byte `FileEntry` values into their final positions.
/// This mirrors upstream rsync's approach of sorting a pointer array
/// (`sorted[]`) rather than moving `file_struct` data.
///
/// When `use_qsort` is true, uses an unstable sort (matching upstream's
/// `--qsort` which uses the C library `qsort()`). When false, uses a
/// stable merge sort (upstream's default).
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` - Called after `send_file_list()`
///   and `recv_file_list()` to sort entries.
/// - `flist.c:2991` - `if (use_qsort) qsort(...); else merge_sort(...);`
pub fn sort_file_list(file_list: &mut [FileEntry], use_qsort: bool) {
    debug_log!(Flist, 2, "sorting {} entries", file_list.len());
    let n = file_list.len();
    if n <= 1 {
        return;
    }

    // Sort indices — only 8-byte values are shuffled during the sort,
    // reducing memory bandwidth by ~20x vs moving full FileEntry structs.
    let mut indices: Vec<usize> = (0..n).collect();
    let cmp = |&a: &usize, &b: &usize| compare_file_entries(&file_list[a], &file_list[b]);
    if use_qsort {
        indices.sort_unstable_by(cmp);
    } else {
        indices.sort_by(cmp);
    }

    // Apply the permutation in-place using cycle chasing.
    // Each element is moved exactly once.
    let mut placed = vec![false; n];
    for i in 0..n {
        if placed[i] || indices[i] == i {
            placed[i] = true;
            continue;
        }
        let mut j = i;
        loop {
            let target = indices[j];
            placed[j] = true;
            if target == i {
                break;
            }
            file_list.swap(j, target);
            j = target;
        }
    }
}

/// Result of cleaning a file list.
#[derive(Debug, Clone, Default)]
pub struct CleanResult {
    /// Number of duplicate entries removed.
    pub duplicates_removed: usize,
    /// Number of directory flags merged.
    pub flags_merged: usize,
}

/// Cleans a sorted file list in-place by removing duplicates and merging directory flags.
///
/// This mirrors upstream's `clean_flist()` which operates in-place with zero allocation,
/// clearing duplicate entries as tombstones rather than building a new list.
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
pub fn flist_clean(mut file_list: Vec<FileEntry>) -> (Vec<FileEntry>, CleanResult) {
    let len = file_list.len();
    if len == 0 {
        return (file_list, CleanResult::default());
    }

    let mut stats = CleanResult::default();

    // Write cursor: position where the next kept entry goes.
    // Read cursor `r` scans ahead. Like upstream's in-place tombstone approach,
    // but we compact immediately so a single truncate suffices.
    let mut w: usize = 0;
    let mut r: usize = 1;

    while r < len {
        if file_list[w].name() != file_list[r].name() {
            w += 1;
            if w != r {
                file_list.swap(w, r);
            }
            r += 1;
            continue;
        }

        // Duplicate found - decide which to keep at position `w`
        let w_is_dir = file_list[w].is_dir();
        let r_is_dir = file_list[r].is_dir();

        match (w_is_dir, r_is_dir) {
            (false, true) => {
                // Keep the directory (at r), replace current write position
                file_list.swap(w, r);
                stats.duplicates_removed += 1;
            }
            (true, false) => {
                // Keep current (directory at w), drop r
                stats.duplicates_removed += 1;
            }
            (true, true) => {
                // Both directories - merge flags into w
                // upstream: flist.c merges FLAG_TOP_DIR, FLAG_CONTENT_DIR
                if file_list[r].content_dir() {
                    file_list[w].set_content_dir(true);
                }
                stats.duplicates_removed += 1;
                stats.flags_merged += 1;
            }
            (false, false) => {
                // Both files - keep first (at w)
                stats.duplicates_removed += 1;
            }
        }

        r += 1;
    }

    file_list.truncate(w + 1);

    debug_log!(
        Flist,
        2,
        "cleaned file list: {} entries, {} duplicates removed, {} flags merged",
        file_list.len(),
        stats.duplicates_removed,
        stats.flags_merged
    );

    (file_list, stats)
}

/// Sorts and cleans a file list in one operation.
///
/// Combines `sort_file_list` and `flist_clean` for convenience.
/// When `use_qsort` is true, uses unstable sort matching upstream `--qsort`.
///
/// # Upstream Reference
///
/// - `flist.c:flist_sort_and_clean()` - The combined operation
#[must_use]
pub fn sort_and_clean_file_list(
    mut file_list: Vec<FileEntry>,
    use_qsort: bool,
) -> (Vec<FileEntry>, CleanResult) {
    sort_file_list(&mut file_list, use_qsort);
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

        sort_file_list(&mut entries, false);

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

        sort_file_list(&mut entries, false);

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
        let (cleaned, stats) = sort_and_clean_file_list(entries, false);
        assert_eq!(stats.duplicates_removed, 1);
        let mut names = Vec::with_capacity(cleaned.len());
        names.extend(cleaned.iter().map(|e| e.name()));
        assert_eq!(names, vec!["a.txt", "z.txt", "a"]);
    }

    /// Comprehensive edge-case sort order golden test.
    ///
    /// Captures the exact expected ordering for tricky cases involving
    /// deep nesting, shared prefixes, file-vs-directory at same level,
    /// and implicit trailing slashes. Any change to the comparator that
    /// alters this ordering would break wire compatibility (NDX mismatch).
    #[test]
    fn sort_order_golden_comprehensive() {
        let mut entries = vec![
            // Root-level files
            make_file("a"),
            make_file("b.txt"),
            make_file("z"),
            // Root-level dirs
            make_dir("a"),
            make_dir("ab"),
            make_dir("b"),
            // Files that share prefix with dirs
            make_file("ab.txt"),
            make_file("a/file.txt"),
            make_file("a/z.txt"),
            // Nested dirs
            make_dir("a/sub"),
            make_file("a/sub/deep.txt"),
            // Dir with name that is prefix of another dir
            make_dir("a/sub/deep"),
            make_file("a/sub/deep/leaf.txt"),
            // Same-depth file vs dir disambiguation
            make_file("b/file.txt"),
            make_dir("b/dir"),
            make_file("b/dir/inner.txt"),
            // "." root marker
            make_dir("."),
            // Paths with shared multi-component prefixes
            make_dir("x/y"),
            make_file("x/y/a.txt"),
            make_file("x/y/b.txt"),
            make_dir("x/y/c"),
            make_file("x/y/c/d.txt"),
            make_file("x/z.txt"),
            make_dir("x"),
        ];

        sort_file_list(&mut entries, false);

        let names: Vec<&str> = entries.iter().map(|e| e.name()).collect();
        assert_eq!(
            names,
            vec![
                ".",
                // Root files before root dirs
                "a",
                "ab.txt",
                "b.txt",
                "z",
                // Root dir "a" and its contents
                "a",          // dir
                "a/file.txt", // file in a/
                "a/z.txt",    // file in a/
                "a/sub",      // dir in a/
                "a/sub/deep.txt",
                "a/sub/deep",          // dir
                "a/sub/deep/leaf.txt", // file in a/sub/deep/
                // Root dir "ab"
                "ab",
                // Root dir "b" and its contents
                "b",
                "b/file.txt",
                "b/dir",
                "b/dir/inner.txt",
                // Root dir "x" and its contents
                "x",
                "x/z.txt",
                "x/y",
                "x/y/a.txt",
                "x/y/b.txt",
                "x/y/c",
                "x/y/c/d.txt",
            ]
        );
    }

    /// Tests that a file named "foo" (non-dir) sorts before dir "foo" at
    /// the same level, because files sort before directories.
    #[test]
    fn file_before_same_name_dir() {
        let file = make_file("item");
        let dir = make_dir("item");
        assert_eq!(compare_file_entries(&file, &dir), Ordering::Less);
        assert_eq!(compare_file_entries(&dir, &file), Ordering::Greater);
    }

    /// Tests deeply nested paths with long shared prefixes.
    #[test]
    fn deep_nesting_shared_prefix() {
        let deep_file = make_file("a/b/c/d/e/f/g.txt");
        let deep_dir = make_dir("a/b/c/d/e/f/g");
        // File sorts before dir with same name
        assert_eq!(compare_file_entries(&deep_file, &deep_dir), Ordering::Less);

        let sibling_file = make_file("a/b/c/d/e/f/h.txt");
        // g.txt < h.txt alphabetically, both are files at same depth
        assert_eq!(
            compare_file_entries(&deep_file, &sibling_file),
            Ordering::Less
        );

        // Dir g/ sorts after file h.txt (dirs after files at same level)
        assert_eq!(
            compare_file_entries(&deep_dir, &sibling_file),
            Ordering::Greater
        );
    }

    /// Verifies that `use_qsort=true` produces the same ordering as the default
    /// stable sort. The comparison function is identical; only sort stability differs.
    #[test]
    fn qsort_flag_produces_correct_order() {
        let entries = vec![
            make_file("test.txt"),
            make_dir("subdir"),
            make_file("subdir/file.txt"),
            make_file("another.txt"),
            make_dir("."),
        ];

        let mut stable_entries = entries.clone();
        sort_file_list(&mut stable_entries, false);

        let mut qsort_entries = entries;
        sort_file_list(&mut qsort_entries, true);

        // With no duplicate keys, both algorithms must produce the same result
        let stable_names: Vec<&str> = stable_entries.iter().map(|e| e.name()).collect();
        let qsort_names: Vec<&str> = qsort_entries.iter().map(|e| e.name()).collect();
        assert_eq!(stable_names, qsort_names);
    }

    /// Verifies that `sort_and_clean_file_list` works with `use_qsort=true`.
    #[test]
    fn sort_and_clean_with_qsort() {
        let entries = vec![
            make_file("z.txt"),
            make_dir("a"),
            make_file("a.txt"),
            make_file("a.txt"), // duplicate
        ];
        let (cleaned, stats) = sort_and_clean_file_list(entries, true);
        assert_eq!(stats.duplicates_removed, 1);
        let names: Vec<&str> = cleaned.iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["a.txt", "z.txt", "a"]);
    }
}
