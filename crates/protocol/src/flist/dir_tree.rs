//! Directory tree for incremental recursion file list exchange.
//!
//! Tracks directories discovered during file list building and provides
//! depth-first traversal for sending sub-lists. Each node corresponds to
//! a directory that needs its contents sent as a separate file list segment.
//!
//! # Upstream Reference
//!
//! - `flist.c:add_dirs_to_tree()` — builds tree from directory entries
//! - `flist.c:send_extra_file_list()` — traverses tree depth-first via
//!   `DIR_FIRST_CHILD`, `DIR_NEXT_SIBLING`, `DIR_PARENT` macros

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
        // Virtual root node — never yielded, serves as parent for top-level dirs
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
            let mut sibling = self.nodes[actual_parent].first_child.unwrap();
            while let Some(next) = self.nodes[sibling].next_sibling {
                sibling = next;
            }
            self.nodes[sibling].next_sibling = Some(node_idx);
        }

        // Set cursor to first real node if not yet set
        if self.cursor.is_none() {
            self.cursor = Some(node_idx);
        }

        node_idx
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
            // No children — go to next sibling, or backtrack to parent's sibling
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
}
