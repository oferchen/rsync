//! Sorting utilities for file list operations.

/// Sorts file list entries by relative path in lexicographic order.
#[cfg(feature = "parallel")]
pub(crate) fn sort_file_entries(entries: &mut [crate::entry::FileListEntry]) {
    entries.sort_unstable_by(|a, b| a.relative_path.cmp(&b.relative_path));
}

/// Removes consecutive entries with identical relative paths, keeping the
/// first occurrence. Mirrors upstream's `flist_sort_and_clean` dedup pass
/// that marks `FLAG_DUPLICATE` so the sender skips repeated entries.
///
/// Requires the slice be sorted by relative path (see
/// [`sort_file_entries`]) so that duplicate sources collapse to adjacent
/// positions.
///
/// upstream: flist.c:3050 dedup_in_flist() - `f_name_cmp() == 0` between
/// consecutive sorted entries flags the second copy as `FLAG_DUPLICATE`;
/// upstream: flist.c:2159-2172 - sender skips entries carrying the flag.
#[cfg(feature = "parallel")]
pub(crate) fn dedup_sorted_file_entries(entries: &mut Vec<crate::entry::FileListEntry>) {
    entries.dedup_by(|a, b| a.relative_path == b.relative_path);
}

/// Sorts directory entries by file name in lexicographic order.
#[cfg(feature = "parallel")]
pub(crate) fn sort_dir_entries(entries: &mut [std::fs::DirEntry]) {
    entries.sort_unstable_by_key(|e| e.file_name());
}

/// Sorts OS strings in lexicographic order.
pub(crate) fn sort_os_strings(entries: &mut [std::ffi::OsString]) {
    entries.sort_unstable();
}

#[cfg(test)]
#[cfg(feature = "parallel")]
mod tests {
    use super::*;
    use crate::entry::FileListEntry;
    use std::fs;
    use std::path::PathBuf;

    fn make_entry(rel: &str, tmp: &std::path::Path) -> FileListEntry {
        let full = tmp.join(rel);
        let metadata = fs::metadata(tmp).expect("metadata");
        FileListEntry {
            full_path: full,
            relative_path: PathBuf::from(rel),
            metadata,
            depth: 1,
            is_root: false,
        }
    }

    /// upstream: flist.c:3050 - identical sorted neighbours collapse to one.
    /// Mirrors `testsuite/duplicates.test` which passes the same source ten
    /// times and asserts each entry appears exactly once in the flist.
    #[test]
    fn dedup_collapses_consecutive_identical_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut entries = vec![
            make_entry("a", tmp.path()),
            make_entry("a", tmp.path()),
            make_entry("a", tmp.path()),
            make_entry("b", tmp.path()),
            make_entry("b", tmp.path()),
            make_entry("c", tmp.path()),
        ];

        dedup_sorted_file_entries(&mut entries);

        let paths: Vec<&std::path::Path> =
            entries.iter().map(|e| e.relative_path.as_path()).collect();
        assert_eq!(
            paths,
            vec![
                std::path::Path::new("a"),
                std::path::Path::new("b"),
                std::path::Path::new("c"),
            ]
        );
    }

    #[test]
    fn dedup_preserves_distinct_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut entries = vec![
            make_entry("a", tmp.path()),
            make_entry("b", tmp.path()),
            make_entry("c", tmp.path()),
        ];

        dedup_sorted_file_entries(&mut entries);

        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn dedup_on_empty_is_noop() {
        let mut entries: Vec<FileListEntry> = Vec::new();
        dedup_sorted_file_entries(&mut entries);
        assert!(entries.is_empty());
    }

    /// Build via the parallel collector with the same root passed multiple
    /// times in the path list (matches `duplicates.test`'s argv shape) and
    /// confirm each discovered entry appears exactly once after sort+dedup.
    #[test]
    fn dedup_collapses_duplicate_source_paths_from_walker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("name1"), b"This is the file").expect("write name1");

        let walker = crate::FileListBuilder::new(root)
            .build()
            .expect("build walker");
        let single: Vec<_> = walker
            .collect::<Result<Vec<_>, _>>()
            .expect("collect single");

        // Simulate three passes through the same source by repeating entries
        // and then sort+dedup, matching the sender pipeline.
        let mut tripled: Vec<FileListEntry> = Vec::new();
        for _ in 0..3 {
            let walker = crate::FileListBuilder::new(root)
                .build()
                .expect("build walker");
            for entry in walker {
                tripled.push(entry.expect("entry"));
            }
        }

        sort_file_entries(&mut tripled);
        dedup_sorted_file_entries(&mut tripled);

        assert_eq!(tripled.len(), single.len());
        let names: Vec<&std::path::Path> =
            tripled.iter().map(|e| e.relative_path.as_path()).collect();
        assert!(names.iter().any(|p| *p == std::path::Path::new("name1")));
    }
}
