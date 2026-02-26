//! Sorting strategy dispatch for file list operations.
//!
//! With the `art` feature, items are inserted into an Adaptive Radix Tree
//! and drained in lexicographic order — O(n * k) where k is key length.
//! Without `art`, a standard `sort_unstable` comparison sort is used — O(n log n).
//!
//! This module centralizes the ART-vs-sort strategy so callers need only a single
//! function call instead of duplicated `#[cfg]` blocks.

/// Sorts file list entries by relative path in lexicographic order.
#[cfg(all(feature = "parallel", feature = "art"))]
pub(crate) fn sort_file_entries(entries: &mut Vec<crate::entry::FileListEntry>) {
    let sorted = entries
        .drain(..)
        .collect::<crate::art_file_list::ArtFileList>()
        .into_sorted_vec();
    *entries = sorted;
}

/// Sorts file list entries by relative path in lexicographic order.
#[cfg(all(feature = "parallel", not(feature = "art")))]
#[allow(clippy::ptr_arg)]
pub(crate) fn sort_file_entries(entries: &mut Vec<crate::entry::FileListEntry>) {
    entries.sort_unstable_by(|a, b| a.relative_path.cmp(&b.relative_path));
}

/// Sorts directory entries by file name in lexicographic order.
#[cfg(all(feature = "parallel", feature = "art"))]
pub(crate) fn sort_dir_entries(entries: &mut Vec<std::fs::DirEntry>) {
    use crate::art_file_list::path_to_art_key;
    use rart::keys::vector_key::VectorKey;
    use rart::tree::AdaptiveRadixTree;

    let mut tree: AdaptiveRadixTree<VectorKey, std::fs::DirEntry> = AdaptiveRadixTree::new();
    for entry in entries.drain(..) {
        let key = path_to_art_key(entry.file_name().as_encoded_bytes());
        tree.insert(key, entry);
    }
    let keys: Vec<VectorKey> = tree.iter().map(|(k, _)| k).collect();
    *entries = keys.into_iter().filter_map(|k| tree.remove_k(&k)).collect();
}

/// Sorts directory entries by file name in lexicographic order.
#[cfg(all(feature = "parallel", not(feature = "art")))]
#[allow(clippy::ptr_arg)]
pub(crate) fn sort_dir_entries(entries: &mut Vec<std::fs::DirEntry>) {
    entries.sort_unstable_by_key(|e| e.file_name());
}

/// Sorts OS strings in lexicographic order.
#[cfg(feature = "art")]
pub(crate) fn sort_os_strings(entries: &mut Vec<std::ffi::OsString>) {
    use crate::art_file_list::path_to_art_key;
    use rart::keys::vector_key::VectorKey;
    use rart::tree::AdaptiveRadixTree;

    let mut tree: AdaptiveRadixTree<VectorKey, std::ffi::OsString> = AdaptiveRadixTree::new();
    for s in entries.drain(..) {
        let key = path_to_art_key(s.as_encoded_bytes());
        tree.insert(key, s);
    }
    *entries = tree.iter().map(|(_, v)| v.clone()).collect();
}

/// Sorts OS strings in lexicographic order.
#[cfg(not(feature = "art"))]
#[allow(clippy::ptr_arg)]
pub(crate) fn sort_os_strings(entries: &mut Vec<std::ffi::OsString>) {
    entries.sort_unstable();
}
