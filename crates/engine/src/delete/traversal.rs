//! [`DirTraversalCursor`] - yields directories in upstream traversal order.
//!
//! The cursor reproduces upstream rsync's depth-first walk over the
//! sender's flist (`generator.c:2282-2354`,
//! `do_delete_pass`/`delete_in_dir`). Each parent's child directories are
//! emitted in `f_name_cmp` ascending order, the root is emitted first,
//! and the cursor descends fully into one subtree before moving on to the
//! next sibling.
//!
//! # Single-Threaded By Construction
//!
//! Only the emitter thread holds a [`DirTraversalCursor`]. The phase-1
//! workers publish their observations via a separate channel; the
//! emitter folds them in by calling [`DirTraversalCursor::observe_segment`]
//! before consuming the corresponding directory.
//!
//! # Observation Order
//!
//! `observe_segment` calls may arrive in any order; the cursor sorts the
//! children of each parent every time the set grows, so out-of-order
//! observation does not affect the emission order. The only constraint is
//! that a directory's children MUST be observed before that directory
//! itself is consumed via [`DirTraversalCursor::next_ready`]. Observing
//! children after the parent has been advanced past is treated as a
//! programmer error; the late children are silently ignored.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use protocol::flist::{FileEntry, FileType, f_name_cmp};

/// One frame on the DFS stack: a directory currently being iterated and
/// the index of its next-to-emit child.
#[derive(Debug, Clone)]
struct Frame {
    dir: PathBuf,
    next_child_ix: usize,
}

/// Yields directories in upstream's depth-first, `f_name_cmp`-ascending
/// order so the emitter drains [`super::DeletePlanMap`] in the same order
/// upstream walks the flist.
///
/// The cursor is single-threaded by construction. The emitter creates one
/// for the transfer root and threads it through phase-2 dispatch.
#[derive(Debug, Clone)]
pub struct DirTraversalCursor {
    /// Destination-relative root directory; emitted first.
    root: PathBuf,
    /// For each parent directory, the sorted list of child directories.
    child_dirs: HashMap<PathBuf, Vec<PathBuf>>,
    /// DFS stack of frames currently being iterated.
    stack: Vec<Frame>,
    /// `true` once the root has been emitted from `next_ready`.
    root_emitted: bool,
}

impl DirTraversalCursor {
    /// Creates a cursor rooted at `root`.
    ///
    /// The first call to [`Self::next_ready`] returns `Some(root)`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            child_dirs: HashMap::new(),
            stack: Vec::new(),
            root_emitted: false,
        }
    }

    /// Records the directory children observed in one flist segment.
    ///
    /// `dir` is the destination-relative parent directory the segment
    /// describes. `children` is the segment's full child set; the cursor
    /// keeps only those entries whose [`FileType`] is
    /// [`FileType::Directory`] and stores each one as a path of the form
    /// `dir.join(child_basename)`.
    ///
    /// Calling `observe_segment` multiple times for the same `dir` is
    /// permitted: children unions, no duplicates, and the stored list is
    /// re-sorted in `f_name_cmp` order. Late observations after the
    /// parent has been advanced past are silently dropped from the
    /// emission sequence (the entry is still recorded in `child_dirs`,
    /// but the iteration stack will not revisit the parent).
    pub fn observe_segment(&mut self, dir: PathBuf, children: &[FileEntry]) {
        let entry = self.child_dirs.entry(dir.clone()).or_default();
        for child in children {
            if !matches!(child.file_type(), FileType::Directory) {
                continue;
            }
            let basename = child_basename(child);
            if basename.is_empty() {
                continue;
            }
            let full = if dir.as_os_str().is_empty() || dir == Path::new(".") {
                PathBuf::from(&basename)
            } else {
                dir.join(&basename)
            };
            if !entry.iter().any(|p| p == &full) {
                entry.push(full);
            }
        }
        sort_paths_by_f_name_cmp(entry);
    }

    /// Returns the next directory whose parent has already been emitted.
    ///
    /// Order matches upstream's depth-first walk: parent before children,
    /// siblings in `f_name_cmp` ascending order. Returns `None` when the
    /// tree is exhausted (root emitted and stack empty).
    ///
    /// # Panics
    ///
    /// Does not panic. Internal invariants are upheld by construction.
    pub fn next_ready(&mut self) -> Option<PathBuf> {
        if !self.root_emitted {
            self.root_emitted = true;
            self.stack.push(Frame {
                dir: self.root.clone(),
                next_child_ix: 0,
            });
            return Some(self.root.clone());
        }

        loop {
            // Inspect the top frame without holding a mutable borrow
            // across the subsequent `self.stack.push` / `pop` calls.
            let (dir, ix) = match self.stack.last() {
                Some(frame) => (frame.dir.clone(), frame.next_child_ix),
                None => return None,
            };
            let next_child = self
                .child_dirs
                .get(&dir)
                .and_then(|list| list.get(ix).cloned());
            match next_child {
                Some(next) => {
                    // Advance the parent's child-iterator, then descend.
                    if let Some(top) = self.stack.last_mut() {
                        top.next_child_ix = ix + 1;
                    }
                    self.stack.push(Frame {
                        dir: next.clone(),
                        next_child_ix: 0,
                    });
                    return Some(next);
                }
                None => {
                    // No more children for this directory; backtrack.
                    self.stack.pop();
                }
            }
        }
    }

    /// Returns `true` when the cursor has emitted everything it can.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.root_emitted && self.stack.is_empty()
    }
}

/// Sorts a list of paths in-place using `f_name_cmp` over a transient
/// [`FileEntry`] per path. Treats each path as a directory entry so the
/// comparator sees the same `dirname` / `basename` split it would for a
/// real flist row.
pub(super) fn sort_paths_by_f_name_cmp(paths: &mut [PathBuf]) {
    paths.sort_unstable_by(|a, b| {
        let ea = FileEntry::new_directory(a.clone(), 0o755);
        let eb = FileEntry::new_directory(b.clone(), 0o755);
        f_name_cmp(&ea, &eb)
    });
}

/// Returns the leaf component of a [`FileEntry`]'s path as an
/// [`OsString`]. Empty when the path itself is empty.
fn child_basename(entry: &FileEntry) -> OsString {
    entry
        .path()
        .file_name()
        .map(OsString::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::flist::FileEntry;

    fn dir_entry(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    fn file_entry(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 0, 0o644)
    }

    #[test]
    fn empty_cursor_yields_only_root() {
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        assert_eq!(cursor.next_ready(), Some(PathBuf::from("root")));
        assert_eq!(cursor.next_ready(), None);
        assert!(cursor.is_exhausted());
    }

    #[test]
    fn single_level_emits_children_in_ascending_order() {
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment(
            PathBuf::from("root"),
            &[
                dir_entry("root/c"),
                dir_entry("root/a"),
                dir_entry("root/b"),
            ],
        );
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(
            seq,
            vec![
                PathBuf::from("root"),
                PathBuf::from("root/a"),
                PathBuf::from("root/b"),
                PathBuf::from("root/c"),
            ]
        );
    }

    #[test]
    fn observe_segments_out_of_order_yields_depth_first_f_name_cmp_order() {
        // Tree:   root
        //         |- a
        //         |   |- x
        //         |   `- y
        //         `- b
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        // Observe in deliberately scrambled order.
        cursor.observe_segment(
            PathBuf::from("root/a"),
            &[dir_entry("root/a/y"), dir_entry("root/a/x")],
        );
        cursor.observe_segment(
            PathBuf::from("root"),
            &[dir_entry("root/b"), dir_entry("root/a")],
        );
        // Re-observing the same parent with the same children is a no-op.
        cursor.observe_segment(PathBuf::from("root/a"), &[dir_entry("root/a/x")]);
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(
            seq,
            vec![
                PathBuf::from("root"),
                PathBuf::from("root/a"),
                PathBuf::from("root/a/x"),
                PathBuf::from("root/a/y"),
                PathBuf::from("root/b"),
            ]
        );
        assert!(cursor.is_exhausted());
    }

    #[test]
    fn non_directory_children_are_ignored() {
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment(
            PathBuf::from("root"),
            &[
                dir_entry("root/sub"),
                file_entry("root/file.txt"),
                FileEntry::new_symlink(PathBuf::from("root/link"), PathBuf::from("target")),
            ],
        );
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(seq, vec![PathBuf::from("root"), PathBuf::from("root/sub")]);
    }

    #[test]
    fn duplicate_observations_do_not_double_emit() {
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment(PathBuf::from("root"), &[dir_entry("root/a")]);
        cursor.observe_segment(PathBuf::from("root"), &[dir_entry("root/a")]);
        cursor.observe_segment(PathBuf::from("root"), &[dir_entry("root/a")]);
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(seq, vec![PathBuf::from("root"), PathBuf::from("root/a")]);
    }

    #[test]
    fn deeper_tree_emits_full_depth_first_order() {
        // root/a/x/q, root/a/x/p, root/a/y, root/b, root/c/m
        let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
        cursor.observe_segment(
            PathBuf::from("root"),
            &[
                dir_entry("root/c"),
                dir_entry("root/a"),
                dir_entry("root/b"),
            ],
        );
        cursor.observe_segment(
            PathBuf::from("root/a"),
            &[dir_entry("root/a/y"), dir_entry("root/a/x")],
        );
        cursor.observe_segment(
            PathBuf::from("root/a/x"),
            &[dir_entry("root/a/x/q"), dir_entry("root/a/x/p")],
        );
        cursor.observe_segment(PathBuf::from("root/c"), &[dir_entry("root/c/m")]);
        let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
        assert_eq!(
            seq,
            vec![
                PathBuf::from("root"),
                PathBuf::from("root/a"),
                PathBuf::from("root/a/x"),
                PathBuf::from("root/a/x/p"),
                PathBuf::from("root/a/x/q"),
                PathBuf::from("root/a/y"),
                PathBuf::from("root/b"),
                PathBuf::from("root/c"),
                PathBuf::from("root/c/m"),
            ]
        );
    }

    #[test]
    fn cursor_handles_root_with_no_observations() {
        let mut cursor = DirTraversalCursor::new(PathBuf::from("."));
        assert!(!cursor.is_exhausted());
        assert_eq!(cursor.next_ready(), Some(PathBuf::from(".")));
        assert_eq!(cursor.next_ready(), None);
        assert!(cursor.is_exhausted());
    }

    #[test]
    fn high_byte_basenames_use_unsigned_byte_order() {
        // f_name_cmp is unsigned-byte; "z" (0x7A) sorts before any name
        // starting with a high byte like 0xC3.
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let high = PathBuf::from(OsStr::from_bytes(b"root/\xC3\xA9_other"));
            let z = PathBuf::from("root/z");
            let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
            cursor.observe_segment(
                PathBuf::from("root"),
                &[
                    FileEntry::new_directory(high.clone(), 0o755),
                    FileEntry::new_directory(z.clone(), 0o755),
                ],
            );
            let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
            assert_eq!(seq, vec![PathBuf::from("root"), z, high]);
        }
        #[cfg(not(unix))]
        {
            // Same property over ASCII names: 'A' (0x41) < 'z' (0x7A).
            let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
            cursor.observe_segment(
                PathBuf::from("root"),
                &[dir_entry("root/z"), dir_entry("root/A")],
            );
            let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
            assert_eq!(
                seq,
                vec![
                    PathBuf::from("root"),
                    PathBuf::from("root/A"),
                    PathBuf::from("root/z"),
                ]
            );
        }
    }
}
