//! Path interning for file list directory names.
//!
//! In rsync file lists, many entries share the same parent directory. This module
//! provides a `PathInterner` that deduplicates directory paths by storing each
//! unique dirname once behind an `Arc<Path>` and sharing references across all
//! entries in that directory.
//!
//! # Memory Savings
//!
//! For a file list with 10,000 files across 100 directories, this reduces dirname
//! allocations from 10,000 `PathBuf` instances to 100 `Arc<Path>` instances shared
//! via reference counting.
//!
//! # Thread Safety
//!
//! `PathInterner` is not `Sync` — it is designed for single-threaded use during
//! sequential file list decoding. The interned `Arc<Path>` values are `Send + Sync`
//! and can be freely shared across threads after interning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Deduplicates directory paths by mapping each unique path to a shared `Arc<Path>`.
///
/// During file list construction, the interner is consulted for each entry's parent
/// directory. If the directory has been seen before, the existing `Arc<Path>` is
/// returned; otherwise a new one is created and cached.
///
/// # Examples
///
/// ```
/// use protocol::flist::PathInterner;
/// use std::path::Path;
/// use std::sync::Arc;
///
/// let mut interner = PathInterner::new();
/// let dir1 = interner.intern(Path::new("src/lib"));
/// let dir2 = interner.intern(Path::new("src/lib"));
/// assert!(Arc::ptr_eq(&dir1, &dir2));
/// ```
#[derive(Debug)]
pub struct PathInterner {
    /// Map from directory path to shared reference.
    map: HashMap<PathBuf, Arc<Path>>,
    /// Cached `Arc<Path>` for the empty path (root-level entries with no dirname).
    empty: Arc<Path>,
}

impl Default for PathInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl PathInterner {
    /// Creates an empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            empty: Arc::from(Path::new("")),
        }
    }

    /// Creates an interner with pre-allocated capacity for the expected number
    /// of unique directories.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity(capacity),
            empty: Arc::from(Path::new("")),
        }
    }

    /// Returns a shared reference to the interned path.
    ///
    /// If `path` has been interned before, the same `Arc<Path>` is returned
    /// (pointer-equal). Otherwise a new `Arc<Path>` is allocated and cached.
    ///
    /// Empty paths are handled specially to avoid a HashMap lookup.
    pub fn intern(&mut self, path: &Path) -> Arc<Path> {
        if path.as_os_str().is_empty() {
            return Arc::clone(&self.empty);
        }

        if let Some(existing) = self.map.get(path) {
            return Arc::clone(existing);
        }

        let arc: Arc<Path> = Arc::from(path);
        self.map.insert(path.to_path_buf(), Arc::clone(&arc));
        arc
    }

    /// Returns the number of unique paths currently interned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns true if no paths have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Removes all interned paths, freeing memory.
    ///
    /// Existing `Arc<Path>` references remain valid until dropped.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn intern_returns_same_arc_for_same_path() {
        let mut interner = PathInterner::new();
        let a = interner.intern(Path::new("src/lib"));
        let b = interner.intern(Path::new("src/lib"));
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn intern_returns_different_arc_for_different_paths() {
        let mut interner = PathInterner::new();
        let a = interner.intern(Path::new("src/lib"));
        let b = interner.intern(Path::new("src/bin"));
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn intern_empty_path() {
        let mut interner = PathInterner::new();
        let a = interner.intern(Path::new(""));
        let b = interner.intern(Path::new(""));
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(interner.len(), 0);
    }

    #[test]
    fn len_tracks_unique_paths() {
        let mut interner = PathInterner::new();
        assert!(interner.is_empty());

        interner.intern(Path::new("a"));
        assert_eq!(interner.len(), 1);

        interner.intern(Path::new("a"));
        assert_eq!(interner.len(), 1);

        interner.intern(Path::new("b"));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn clear_removes_entries() {
        let mut interner = PathInterner::new();
        interner.intern(Path::new("a"));
        interner.intern(Path::new("b"));
        assert_eq!(interner.len(), 2);

        interner.clear();
        assert!(interner.is_empty());
    }

    #[test]
    fn with_capacity_works() {
        let interner = PathInterner::with_capacity(100);
        assert!(interner.is_empty());
    }

    #[test]
    fn interned_paths_are_correct() {
        let mut interner = PathInterner::new();
        let path = interner.intern(Path::new("deeply/nested/dir"));
        assert_eq!(&*path, Path::new("deeply/nested/dir"));
    }

    #[test]
    fn default_creates_empty_interner() {
        let interner = PathInterner::default();
        assert!(interner.is_empty());
    }

    #[test]
    fn many_paths_with_shared_prefix() {
        let mut interner = PathInterner::new();
        let arcs: Vec<_> = (0..100)
            .map(|i| interner.intern(Path::new(&format!("dir_{}", i / 10))))
            .collect();

        // Every 10 entries should share the same Arc
        for chunk in arcs.chunks(10) {
            for arc in &chunk[1..] {
                assert!(Arc::ptr_eq(&chunk[0], arc));
            }
        }
        assert_eq!(interner.len(), 10);
    }
}
