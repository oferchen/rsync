//! Directory tree for incremental recursion file list exchange.
//!
//! Tracks directories discovered during file list building and provides
//! depth-first traversal for sending sub-lists. Each node corresponds to
//! a directory that needs its contents sent as a separate file list segment.
//!
//! # Upstream Reference
//!
//! - `flist.c:add_dirs_to_tree()` - builds tree from directory entries
//! - `flist.c:send_extra_file_list()` - traverses tree depth-first via
//!   `DIR_FIRST_CHILD`, `DIR_NEXT_SIBLING`, `DIR_PARENT` macros
//!
//! # Security
//!
//! Upstream rsync CVE-2026-43620 (OOB read in `recv_files` via a malformed
//! `parent_ndx`) targets the equivalent indexing performed here. oc-rsync
//! consumes the parent reference as `Option<usize>` and indexes into a
//! bounds-checked `Vec`, so any out-of-range value either returns
//! [`DirTreeError::OutOfBoundsParent`] via [`DirectoryTree::try_add_directory`]
//! or aborts with a Rust bounds-check panic (no UB, no SIGSEGV).

use thiserror::Error;

/// Errors produced by [`DirectoryTree`] when validating wire-supplied indices.
///
/// Created in response to CVE-2026-43620: a peer that supplies a parent index
/// outside the known node table must be rejected, not silently accepted nor
/// allowed to dereference past the end of the underlying storage.
#[derive(Debug, Clone, Eq, PartialEq, Error)]
pub enum DirTreeError {
    /// The parent node index is outside the tree's current node table.
    ///
    /// `parent_idx` is the offending index supplied by the caller (or peer),
    /// and `node_count` is the number of nodes currently in the tree (including
    /// the virtual root). The condition `parent_idx >= node_count` indicates a
    /// malformed or malicious wire payload and the operation is rejected.
    #[error(
        "malformed file list: parent_node_idx={parent_idx} is out of range (node_count={node_count})"
    )]
    OutOfBoundsParent {
        /// The out-of-range parent index that was rejected.
        parent_idx: usize,
        /// The number of valid node indices at the time of rejection.
        node_count: usize,
    },
}

/// A node in the directory tree representing a directory whose contents
/// may need to be sent as a separate file list segment.
#[derive(Debug)]
struct DirNode {
    /// Global NDX of this directory in the directory file list.
    dir_ndx: usize,
    /// Relative path of this directory.
    path: String,
    /// Index of the first child directory (in the `nodes` Vec), or `None`.
    first_child: Option<usize>,
    /// Index of the next sibling directory, or `None`.
    next_sibling: Option<usize>,
    /// Index of the parent directory, or `None` for root.
    parent: Option<usize>,
    /// Whether this directory's sub-list has been sent.
    sent: bool,
}

/// Depth-first directory tree for incremental file list sending.
///
/// Mirrors upstream's `dir_flist` tree structure built by `add_dirs_to_tree()`.
/// Directories are added as they're discovered during file list scanning,
/// and traversed depth-first during the transfer to send sub-lists.
///
/// Internally uses a virtual root node (index 0) that is never yielded.
/// All top-level directories are children of this root.
#[derive(Debug)]
pub struct DirectoryTree {
    /// All directory nodes, indexed by their position in this Vec.
    /// Node 0 is the virtual root (never yielded by `next_directory`).
    nodes: Vec<DirNode>,
    /// Current traversal position (node index).
    cursor: Option<usize>,
    /// Current traversal depth.
    depth: usize,
}

impl DirectoryTree {
    /// Creates a new empty directory tree with a virtual root node.
    #[must_use]
    pub fn new() -> Self {
        // Virtual root node - never yielded, serves as parent for top-level dirs
        let root = DirNode {
            dir_ndx: usize::MAX,
            path: String::new(),
            first_child: None,
            next_sibling: None,
            parent: None,
            sent: true, // marked sent so it's never yielded
        };
        Self {
            nodes: vec![root],
            cursor: None,
            depth: 0,
        }
    }

    /// Virtual root node index.
    const ROOT: usize = 0;

    /// Adds a directory to the tree under the given parent.
    ///
    /// `dir_ndx` is the global index of this directory in the dir_flist.
    /// `parent_node_idx` is the index in this tree's `nodes` Vec (not the global NDX).
    /// Pass `None` for top-level directories (they become children of the virtual root).
    pub fn add_directory(
        &mut self,
        dir_ndx: usize,
        path: String,
        parent_node_idx: Option<usize>,
    ) -> usize {
        let actual_parent = parent_node_idx.unwrap_or(Self::ROOT);
        let node_idx = self.nodes.len();
        self.nodes.push(DirNode {
            dir_ndx,
            path,
            first_child: None,
            next_sibling: None,
            parent: Some(actual_parent),
            sent: false,
        });

        // Add as child of parent: append to end of sibling chain
        if self.nodes[actual_parent].first_child.is_none() {
            self.nodes[actual_parent].first_child = Some(node_idx);
        } else {
            // Walk to last sibling
            let mut sibling = self.nodes[actual_parent]
                .first_child
                .expect("parent has a first child during sibling walk");
            while let Some(next) = self.nodes[sibling].next_sibling {
                sibling = next;
            }
            self.nodes[sibling].next_sibling = Some(node_idx);
        }

        if self.cursor.is_none() {
            self.cursor = Some(node_idx);
        }

        node_idx
    }

    /// Adds a directory under `parent_node_idx`, validating the index first.
    ///
    /// Behaves like [`add_directory`](Self::add_directory) but returns
    /// [`DirTreeError::OutOfBoundsParent`] instead of panicking when a wire-
    /// supplied parent index falls outside the current node table. Use this
    /// entry point whenever the parent index originates from peer input.
    ///
    /// `None` is always accepted: top-level directories become children of the
    /// virtual root node at index 0.
    ///
    /// # Security
    ///
    /// Mitigates the OOB-read attack class described in upstream CVE-2026-43620
    /// by rejecting malformed `parent_ndx` values up front rather than relying
    /// on the panic that the unchecked indexing in `add_directory` would
    /// otherwise produce.
    pub fn try_add_directory(
        &mut self,
        dir_ndx: usize,
        path: String,
        parent_node_idx: Option<usize>,
    ) -> Result<usize, DirTreeError> {
        if let Some(idx) = parent_node_idx
            && idx >= self.nodes.len()
        {
            return Err(DirTreeError::OutOfBoundsParent {
                parent_idx: idx,
                node_count: self.nodes.len(),
            });
        }
        Ok(self.add_directory(dir_ndx, path, parent_node_idx))
    }

    /// Returns the next directory to send (depth-first traversal).
    ///
    /// Returns `(dir_ndx, path)` for the next unsent directory, advancing
    /// the cursor. Returns `None` when all directories have been sent.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors the traversal in `send_extra_file_list()` using
    /// `DIR_FIRST_CHILD`, `DIR_NEXT_SIBLING`, `DIR_PARENT` macros.
    pub fn next_directory(&mut self) -> Option<(usize, &str)> {
        let cursor = self.cursor?;

        // Mark current as sent
        self.nodes[cursor].sent = true;
        let dir_ndx = self.nodes[cursor].dir_ndx;

        // Advance cursor: depth-first (child → sibling → parent's sibling)
        if let Some(child) = self.nodes[cursor].first_child {
            self.cursor = Some(child);
            self.depth += 1;
        } else {
            // No children - go to next sibling, or backtrack to parent's sibling
            let mut current = cursor;
            loop {
                if let Some(sibling) = self.nodes[current].next_sibling {
                    self.cursor = Some(sibling);
                    break;
                }
                // Backtrack to parent
                if let Some(parent) = self.nodes[current].parent {
                    current = parent;
                    self.depth = self.depth.saturating_sub(1);
                } else {
                    // Exhausted all directories
                    self.cursor = None;
                    break;
                }
            }
        }

        Some((dir_ndx, &self.nodes[cursor].path))
    }
}

#[cfg(test)]
impl DirectoryTree {
    /// Returns `true` when all directories have been sent.
    fn is_exhausted(&self) -> bool {
        self.cursor.is_none()
    }

    /// Returns the number of directories in the tree (excluding virtual root).
    #[must_use]
    fn len(&self) -> usize {
        self.nodes.len() - 1 // Exclude virtual root
    }

    /// Returns `true` if the tree has no directories (excluding virtual root).
    #[must_use]
    fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Finds the node index for a directory by its path.
    ///
    /// Used when adding child directories to find the parent node.
    /// Skips the virtual root node at index 0.
    fn find_by_path(&self, path: &str) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .skip(1) // Skip virtual root
            .find(|(_, n)| n.path == path)
            .map(|(i, _)| i)
    }

    /// Finds the node index for a directory by its parent path.
    ///
    /// Looks up the parent directory of `child_path` in the tree.
    fn find_parent_of(&self, child_path: &str) -> Option<usize> {
        use std::path::Path;
        let parent_path = Path::new(child_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if parent_path.is_empty() || parent_path == "." {
            return None; // Top-level directory
        }

        self.find_by_path(&parent_path)
    }
}

impl Default for DirectoryTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tree_is_exhausted() {
        let tree = DirectoryTree::new();
        assert!(tree.is_exhausted());
        assert!(tree.is_empty());
    }

    #[test]
    fn single_directory_traversal() {
        let mut tree = DirectoryTree::new();
        tree.add_directory(0, "subdir".to_string(), None);

        assert!(!tree.is_exhausted());
        let (ndx, path) = tree.next_directory().unwrap();
        assert_eq!(ndx, 0);
        assert_eq!(path, "subdir");
        assert!(tree.is_exhausted());
    }

    #[test]
    fn flat_sibling_traversal() {
        let mut tree = DirectoryTree::new();
        tree.add_directory(0, "a".to_string(), None);
        tree.add_directory(1, "b".to_string(), None);
        tree.add_directory(2, "c".to_string(), None);

        let dirs: Vec<_> =
            std::iter::from_fn(|| tree.next_directory().map(|(n, p)| (n, p.to_string()))).collect();
        assert_eq!(
            dirs,
            vec![
                (0, "a".to_string()),
                (1, "b".to_string()),
                (2, "c".to_string()),
            ]
        );
        assert!(tree.is_exhausted());
    }

    #[test]
    fn depth_first_traversal() {
        let mut tree = DirectoryTree::new();
        // Build: a/ → a/x/, a/y/; b/
        let a = tree.add_directory(0, "a".to_string(), None);
        let _b = tree.add_directory(1, "b".to_string(), None);
        tree.add_directory(2, "a/x".to_string(), Some(a));
        tree.add_directory(3, "a/y".to_string(), Some(a));

        let dirs: Vec<_> =
            std::iter::from_fn(|| tree.next_directory().map(|(n, p)| (n, p.to_string()))).collect();

        // Depth-first: a, a/x, a/y, b
        assert_eq!(
            dirs,
            vec![
                (0, "a".to_string()),
                (2, "a/x".to_string()),
                (3, "a/y".to_string()),
                (1, "b".to_string()),
            ]
        );
    }

    #[test]
    fn deep_nesting_traversal() {
        let mut tree = DirectoryTree::new();
        // Build: a/ → a/b/ → a/b/c/
        let a = tree.add_directory(0, "a".to_string(), None);
        let ab = tree.add_directory(1, "a/b".to_string(), Some(a));
        tree.add_directory(2, "a/b/c".to_string(), Some(ab));

        let dirs: Vec<_> =
            std::iter::from_fn(|| tree.next_directory().map(|(n, p)| (n, p.to_string()))).collect();

        assert_eq!(
            dirs,
            vec![
                (0, "a".to_string()),
                (1, "a/b".to_string()),
                (2, "a/b/c".to_string()),
            ]
        );
    }

    #[test]
    fn find_by_path_and_parent() {
        let mut tree = DirectoryTree::new();
        let a = tree.add_directory(0, "a".to_string(), None);
        tree.add_directory(1, "a/b".to_string(), Some(a));

        assert_eq!(tree.find_by_path("a"), Some(a));
        let ab = tree.find_by_path("a/b").unwrap();
        assert_eq!(ab, a + 1);
        assert_eq!(tree.find_by_path("c"), None);

        assert_eq!(tree.find_parent_of("a/b/file.txt"), Some(ab));
        assert_eq!(tree.find_parent_of("a/file.txt"), Some(a));
        assert_eq!(tree.find_parent_of("file.txt"), None);
    }

    #[test]
    fn len_and_is_empty() {
        let mut tree = DirectoryTree::new();
        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());

        tree.add_directory(0, "a".to_string(), None);
        assert_eq!(tree.len(), 1);
        assert!(!tree.is_empty());
    }

    // CVE-2026-43620 regression coverage (SEC-4).
    //
    // Upstream rsync 3.4.3 fixed an OOB read in `recv_files` triggered by a
    // malformed `parent_ndx` arriving in a file-list segment. The equivalent
    // index in oc-rsync is the `parent_node_idx` consumed by
    // `DirectoryTree::add_directory`. These tests pin down two guarantees:
    //
    // 1. The validating entry point (`try_add_directory`) rejects an
    //    out-of-range parent without mutating the tree.
    // 2. The unchecked entry point (`add_directory`) panics with a Rust
    //    bounds-check rather than reading past the end of the node table,
    //    so the worst-case behaviour is a controlled abort, never UB or
    //    SIGSEGV.

    #[test]
    fn try_add_directory_rejects_out_of_range_parent_idx() {
        // Build a small known-good node table (virtual root + 4 entries).
        let mut tree = DirectoryTree::new();
        let a = tree.add_directory(0, "a".to_string(), None);
        let _b = tree.add_directory(1, "b".to_string(), None);
        let _ax = tree.add_directory(2, "a/x".to_string(), Some(a));
        let _ay = tree.add_directory(3, "a/y".to_string(), Some(a));

        let node_count_before = tree.nodes.len();
        assert_eq!(node_count_before, 5, "virtual root + 4 added directories");

        // A malicious peer sends a parent_node_idx well past the known table.
        let bad_idx: usize = 100;
        let err = tree
            .try_add_directory(99, "evil".to_string(), Some(bad_idx))
            .expect_err("OOB parent_node_idx must be rejected");

        assert_eq!(
            err,
            DirTreeError::OutOfBoundsParent {
                parent_idx: bad_idx,
                node_count: node_count_before,
            }
        );

        // Tree state must be untouched by the rejected insertion.
        assert_eq!(tree.nodes.len(), node_count_before);
    }

    #[test]
    fn try_add_directory_rejects_boundary_off_by_one() {
        // Equal-to-len is still out of range (valid indices are 0..len).
        let mut tree = DirectoryTree::new();
        tree.add_directory(0, "a".to_string(), None);
        let len = tree.nodes.len();

        let err = tree
            .try_add_directory(1, "b".to_string(), Some(len))
            .expect_err("parent_idx == len must be rejected");

        assert!(matches!(
            err,
            DirTreeError::OutOfBoundsParent { parent_idx, node_count }
                if parent_idx == len && node_count == len
        ));
    }

    #[test]
    fn try_add_directory_accepts_valid_parent_and_none() {
        let mut tree = DirectoryTree::new();

        // None always succeeds (top-level under virtual root).
        let a = tree
            .try_add_directory(0, "a".to_string(), None)
            .expect("None is always valid");

        // A previously returned handle is in range.
        let child = tree
            .try_add_directory(1, "a/b".to_string(), Some(a))
            .expect("valid parent index must succeed");

        assert_eq!(child, a + 1);
    }

    #[test]
    fn add_directory_panics_safely_on_oob_parent_idx() {
        // Even the unchecked path must fail in a controlled way: Rust's
        // bounds-check panic is acceptable (no UB), only silent acceptance
        // would be a bug. Captured via `catch_unwind` so the test reports
        // success when the panic fires and failure if the call returns.
        use std::panic;

        let result = panic::catch_unwind(|| {
            let mut tree = DirectoryTree::new();
            tree.add_directory(0, "a".to_string(), None);
            // Bad parent_node_idx far past the end of `nodes`.
            tree.add_directory(99, "evil".to_string(), Some(100));
        });

        assert!(
            result.is_err(),
            "add_directory must panic on OOB parent_node_idx, never silently corrupt state"
        );
    }
}
