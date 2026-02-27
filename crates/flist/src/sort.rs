//! Sorting utilities for file list operations.

/// Sorts file list entries by relative path in lexicographic order.
#[cfg(feature = "parallel")]
#[allow(clippy::ptr_arg)]
pub(crate) fn sort_file_entries(entries: &mut Vec<crate::entry::FileListEntry>) {
    entries.sort_unstable_by(|a, b| a.relative_path.cmp(&b.relative_path));
}

/// Sorts directory entries by file name in lexicographic order.
#[cfg(feature = "parallel")]
#[allow(clippy::ptr_arg)]
pub(crate) fn sort_dir_entries(entries: &mut Vec<std::fs::DirEntry>) {
    entries.sort_unstable_by_key(|e| e.file_name());
}

/// Sorts OS strings in lexicographic order.
#[allow(clippy::ptr_arg)]
pub(crate) fn sort_os_strings(entries: &mut Vec<std::ffi::OsString>) {
    entries.sort_unstable();
}
