//! Sorting utilities for file list operations.

/// Sorts file list entries by relative path in lexicographic order.
#[cfg(feature = "parallel")]
pub(crate) fn sort_file_entries(entries: &mut [crate::entry::FileListEntry]) {
    entries.sort_unstable_by(|a, b| a.relative_path.cmp(&b.relative_path));
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
