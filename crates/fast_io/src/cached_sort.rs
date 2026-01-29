//! Cached sorting using the Schwartzian transform.
//!
//! This module provides utilities for sorting with cached comparison keys,
//! which can significantly speed up sorts where key extraction is expensive.
//!
//! # Background
//!
//! The Schwartzian transform (also known as "decorate-sort-undecorate") avoids
//! redundant key computations during sorting. This is implemented using the
//! [`permutation`](https://crates.io/crates/permutation) crate.
//!
//! # When to Use
//!
//! This is particularly effective when:
//! - Key extraction involves string operations (UTF-8 validation, case conversion)
//! - Key extraction requires I/O or system calls
//! - The comparison function is called O(n log n) times

use std::cmp::Ordering;

pub use permutation::Permutation;

/// A sort key that can be cached and compared efficiently.
///
/// Implement this trait for types that will be used as sort keys.
pub trait CachedSortKey: Ord + Clone {}

// Blanket implementation for common key types
impl CachedSortKey for String {}
impl CachedSortKey for Vec<u8> {}
impl CachedSortKey for i64 {}
impl CachedSortKey for u64 {}
impl CachedSortKey for i32 {}
impl CachedSortKey for u32 {}

/// Sorts a slice in-place using cached sort keys (Schwartzian transform).
///
/// This is more efficient than `sort_by` when key extraction is expensive,
/// as each element's key is computed exactly once.
///
/// # Arguments
///
/// * `items` - The slice to sort
/// * `key_fn` - Function to extract the sort key from each element
///
/// # Example
///
/// ```
/// use fast_io::cached_sort_by;
///
/// let mut items = vec!["Banana", "apple", "Cherry"];
/// cached_sort_by(&mut items, |s| s.to_lowercase());
/// assert_eq!(items, vec!["apple", "Banana", "Cherry"]);
/// ```
pub fn cached_sort_by<T, K, F>(items: &mut [T], key_fn: F)
where
    K: Ord,
    F: Fn(&T) -> K,
{
    if items.len() <= 1 {
        return;
    }

    // Extract keys once
    let keys: Vec<K> = items.iter().map(&key_fn).collect();

    // Sort indices by keys using permutation crate
    let mut perm = permutation::sort_by(&keys, |a, b| a.cmp(b));

    // Apply permutation to items
    perm.apply_slice_in_place(items);
}

/// Sorts a slice in-place using a cached comparison function.
///
/// Similar to `cached_sort_by` but allows custom comparison of keys.
///
/// # Arguments
///
/// * `items` - The slice to sort
/// * `key_fn` - Function to extract the sort key from each element
/// * `cmp_fn` - Function to compare two keys
pub fn cached_sort_by_cmp<T, K, F, C>(items: &mut [T], key_fn: F, cmp_fn: C)
where
    F: Fn(&T) -> K,
    C: Fn(&K, &K) -> Ordering,
{
    if items.len() <= 1 {
        return;
    }

    let keys: Vec<K> = items.iter().map(&key_fn).collect();
    let mut perm = permutation::sort_by(&keys, cmp_fn);
    perm.apply_slice_in_place(items);
}

/// Sorts a slice in parallel using cached sort keys.
///
/// Uses rayon for parallel key extraction.
///
/// # Arguments
///
/// * `items` - The slice to sort
/// * `key_fn` - Thread-safe function to extract the sort key
pub fn cached_sort_by_parallel<T, K, F>(items: &mut [T], key_fn: F)
where
    T: Send + Sync,
    K: Ord + Send,
    F: Fn(&T) -> K + Sync,
{
    use rayon::prelude::*;

    if items.len() <= 1 {
        return;
    }

    // Parallel key extraction
    let keys: Vec<K> = items.par_iter().map(&key_fn).collect();

    // Sort indices by keys
    let mut perm = permutation::sort_by(&keys, |a, b| a.cmp(b));

    // Apply permutation
    perm.apply_slice_in_place(items);
}

/// Pre-computed sort key for file entries.
///
/// Caches the byte representation and directory status to avoid
/// repeated string operations during comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntrySortKey {
    /// The file path as bytes (avoids UTF-8 revalidation).
    pub path_bytes: Vec<u8>,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

impl FileEntrySortKey {
    /// Creates a new sort key from raw path bytes and directory flag.
    ///
    /// This is the preferred constructor as it avoids UTF-8 validation.
    #[must_use]
    pub fn from_bytes(path_bytes: &[u8], is_dir: bool) -> Self {
        Self {
            path_bytes: path_bytes.to_vec(),
            is_dir,
        }
    }

    /// Creates a new sort key from a path string and directory flag.
    ///
    /// For better performance, prefer [`from_bytes`](Self::from_bytes) when
    /// the path is already available as bytes.
    #[must_use]
    pub fn new(path: &str, is_dir: bool) -> Self {
        Self::from_bytes(path.as_bytes(), is_dir)
    }
}

impl Ord for FileEntrySortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // "." always comes first
        match (self.path_bytes.as_slice(), other.path_bytes.as_slice()) {
            (b".", b".") => return Ordering::Equal,
            (b".", _) => return Ordering::Less,
            (_, b".") => return Ordering::Greater,
            _ => {}
        }

        // Compare byte by byte with implicit '/' for directories
        let mut i = 0;
        loop {
            let ch_a = self.effective_byte_at(i);
            let ch_b = other.effective_byte_at(i);

            let a_done = self.is_done_at(i);
            let b_done = other.is_done_at(i);

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
                // Check if we're at a directory boundary
                let remaining_a = &self.path_bytes[i..];
                let remaining_b = &other.path_bytes[i..];

                let a_has_sep = remaining_a.contains(&b'/');
                let b_has_sep = remaining_b.contains(&b'/');

                let a_is_dir_here = a_has_sep || self.is_dir;
                let b_is_dir_here = b_has_sep || other.is_dir;

                // Files sort before directories at same level
                match (a_is_dir_here, b_is_dir_here) {
                    (true, false) => return Ordering::Greater,
                    (false, true) => return Ordering::Less,
                    _ => {}
                }

                return ch_a.cmp(&ch_b);
            }

            i += 1;
        }
    }
}

impl PartialOrd for FileEntrySortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FileEntrySortKey {
    fn effective_byte_at(&self, i: usize) -> u8 {
        if i < self.path_bytes.len() {
            self.path_bytes[i]
        } else if i == self.path_bytes.len() && self.is_dir {
            b'/'
        } else {
            0
        }
    }

    fn is_done_at(&self, i: usize) -> bool {
        i > self.path_bytes.len() || (i == self.path_bytes.len() && !self.is_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_sort_basic() {
        let mut items = vec![3, 1, 4, 1, 5, 9, 2, 6];
        cached_sort_by(&mut items, |&x| x);
        assert_eq!(items, vec![1, 1, 2, 3, 4, 5, 6, 9]);
    }

    #[test]
    fn cached_sort_by_key() {
        let mut items = vec!["Banana", "apple", "Cherry"];
        cached_sort_by(&mut items, |s| s.to_lowercase());
        assert_eq!(items, vec!["apple", "Banana", "Cherry"]);
    }

    #[test]
    fn cached_sort_parallel() {
        let mut items: Vec<i32> = (0..1000).rev().collect();
        cached_sort_by_parallel(&mut items, |&x| x);
        assert_eq!(items, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn file_entry_sort_key_dot_first() {
        let dot = FileEntrySortKey::new(".", true);
        let file = FileEntrySortKey::new("abc.txt", false);
        assert_eq!(dot.cmp(&file), Ordering::Less);
    }

    #[test]
    fn file_entry_sort_key_files_before_dirs() {
        let file = FileEntrySortKey::new("zebra.txt", false);
        let dir = FileEntrySortKey::new("aardvark", true);
        assert_eq!(file.cmp(&dir), Ordering::Less);
    }

    #[test]
    fn file_entry_sort_key_alphabetical_files() {
        let a = FileEntrySortKey::new("a.txt", false);
        let b = FileEntrySortKey::new("b.txt", false);
        assert_eq!(a.cmp(&b), Ordering::Less);
    }

    #[test]
    fn file_entry_sort_key_from_bytes() {
        // Test that from_bytes produces the same result as new()
        let from_str = FileEntrySortKey::new("test/path.txt", false);
        let from_bytes = FileEntrySortKey::from_bytes(b"test/path.txt", false);
        assert_eq!(from_str, from_bytes);
    }

    #[test]
    fn file_entry_sort_key_from_bytes_non_utf8() {
        // Test with non-UTF-8 bytes (valid in file paths on Unix)
        let key = FileEntrySortKey::from_bytes(&[0x80, 0x81, 0x82], false);
        assert_eq!(key.path_bytes, vec![0x80, 0x81, 0x82]);
    }

    #[test]
    fn file_entry_sort_key_from_bytes_comparison() {
        // Test that byte comparison works correctly
        let a = FileEntrySortKey::from_bytes(b"aaa", false);
        let b = FileEntrySortKey::from_bytes(b"bbb", false);
        assert_eq!(a.cmp(&b), Ordering::Less);
    }
}
