//! ART-backed file list with O(k) insertion and prefix range queries.
//!
//! [`ArtFileList`] replaces `Vec<FileListEntry>` for sorted file list storage.
//! Entries are stored in an [`AdaptiveRadixTree`] keyed by relative path bytes,
//! providing lexicographic iteration without an explicit sort pass and O(prefix_len +
//! result_count) subtree queries via [`prefix_range`](ArtFileList::prefix_range).

use std::ops::RangeBounds;

use rart::keys::vector_key::VectorKey;
use rart::tree::AdaptiveRadixTree;

use crate::entry::FileListEntry;

/// Creates a null-terminated `VectorKey` from raw path bytes.
///
/// ART requires that no key is a prefix of another key. Appending a null
/// terminator guarantees this property for filesystem paths, which never
/// contain interior NUL bytes.
pub(crate) fn path_to_art_key(bytes: &[u8]) -> VectorKey {
    let mut v = Vec::with_capacity(bytes.len() + 1);
    v.extend_from_slice(bytes);
    v.push(0);
    VectorKey::new_from_vec(v)
}

/// Ordered file list backed by an Adaptive Radix Tree.
///
/// Entries are keyed by their `relative_path` encoded as bytes. Iteration
/// yields entries in lexicographic order without requiring an explicit sort.
pub struct ArtFileList(AdaptiveRadixTree<VectorKey, FileListEntry>);

impl ArtFileList {
    /// Creates an empty file list.
    #[must_use]
    pub fn new() -> Self {
        Self(AdaptiveRadixTree::new())
    }

    /// Inserts a file entry, keyed by its relative path.
    ///
    /// Returns the previous entry if one existed with the same relative path.
    pub fn insert(&mut self, entry: FileListEntry) -> Option<FileListEntry> {
        let key = path_to_art_key(entry.relative_path.as_os_str().as_encoded_bytes());
        self.0.insert(key, entry)
    }

    /// Returns a reference to the entry at the given relative path, if present.
    #[must_use]
    pub fn get(&self, relative_path: &std::path::Path) -> Option<&FileListEntry> {
        let key = path_to_art_key(relative_path.as_os_str().as_encoded_bytes());
        self.0.get_k(&key)
    }

    /// In-order iterator over all entries.
    ///
    /// Yields `(VectorKey, &FileListEntry)` pairs in lexicographic key order,
    /// replacing the `Vec` iteration pattern used without the `art` feature.
    pub fn iter(&self) -> impl Iterator<Item = (VectorKey, &FileListEntry)> {
        self.0.iter()
    }

    /// Prefix range query — O(prefix_len + result_count).
    ///
    /// Returns all entries whose relative path falls within `range`,
    /// replacing O(n) linear scans for directory-subtree operations.
    pub fn prefix_range<'a, R: RangeBounds<VectorKey> + 'a>(
        &'a self,
        range: R,
    ) -> impl Iterator<Item = (VectorKey, &'a FileListEntry)> {
        self.0.range(range)
    }

    /// Returns `true` if the file list contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Drains all entries in sorted order into a `Vec`.
    ///
    /// This consumes entries from the tree by collecting keys in order
    /// then removing each, yielding owned `FileListEntry` values.
    pub fn into_sorted_vec(mut self) -> Vec<FileListEntry> {
        let keys: Vec<VectorKey> = self.0.iter().map(|(k, _)| k).collect();
        keys.into_iter()
            .filter_map(|k| self.0.remove_k(&k))
            .collect()
    }
}

impl Default for ArtFileList {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<FileListEntry> for ArtFileList {
    fn from_iter<I: IntoIterator<Item = FileListEntry>>(iter: I) -> Self {
        let mut list = Self::new();
        for entry in iter {
            list.insert(entry);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_entry(relative: &str) -> FileListEntry {
        let temp = tempfile::tempdir().unwrap();
        let full = temp.path().to_path_buf();
        let metadata = std::fs::metadata(temp.path()).unwrap();
        // temp is dropped here but the metadata snapshot remains valid
        FileListEntry {
            full_path: full,
            relative_path: PathBuf::from(relative),
            metadata,
            depth: relative.matches('/').count(),
            is_root: relative.is_empty(),
        }
    }

    #[test]
    fn art_file_list_iteration_is_sorted() {
        let mut list = ArtFileList::new();
        list.insert(make_entry("z/file.txt"));
        list.insert(make_entry("a/file.txt"));
        list.insert(make_entry("m/file.txt"));

        let paths: Vec<String> = list
            .iter()
            .map(|(_, e)| e.relative_path().to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, vec!["a/file.txt", "m/file.txt", "z/file.txt"]);
    }

    #[test]
    fn art_file_list_into_sorted_vec() {
        let mut list = ArtFileList::new();
        list.insert(make_entry("c"));
        list.insert(make_entry("a"));
        list.insert(make_entry("b"));

        let vec = list.into_sorted_vec();
        let paths: Vec<&str> = vec
            .iter()
            .map(|e| e.relative_path().to_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["a", "b", "c"]);
    }

    #[test]
    fn art_file_list_get_by_path() {
        let mut list = ArtFileList::new();
        list.insert(make_entry("foo/bar.txt"));

        assert!(list.get(std::path::Path::new("foo/bar.txt")).is_some());
        assert!(list.get(std::path::Path::new("nonexistent")).is_none());
    }

    #[test]
    fn art_file_list_from_iterator() {
        let entries = vec![make_entry("b"), make_entry("a")];
        let list: ArtFileList = entries.into_iter().collect();

        let paths: Vec<String> = list
            .iter()
            .map(|(_, e)| e.relative_path().to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, vec!["a", "b"]);
    }

    #[test]
    fn art_file_list_empty() {
        let list = ArtFileList::new();
        assert!(list.is_empty());
    }

    #[test]
    fn art_file_list_default() {
        let list = ArtFileList::default();
        assert!(list.is_empty());
    }
}
