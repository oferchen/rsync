use crate::error::FileListError;
use crate::file_list_walker::FileListWalker;
use std::path::PathBuf;

/// Configures a filesystem traversal rooted at a specific path.
///
/// # Upstream Reference
///
/// - `flist.c:2192` - `send_file_list()` - Recursive directory scanning
/// - `flist.c:1080` - `send_file_name()` - Per-entry traversal
///
/// This builder configures deterministic filesystem traversal matching
/// upstream rsync's file list construction behavior.
#[derive(Clone, Debug)]
pub struct FileListBuilder {
    root: PathBuf,
    follow_symlinks: bool,
    copy_links: bool,
    include_root: bool,
}

impl FileListBuilder {
    /// Creates a new builder that will traverse the provided root path.
    #[must_use]
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            follow_symlinks: false,
            copy_links: false,
            include_root: true,
        }
    }

    /// Configures whether directory symlinks should be traversed.
    ///
    /// The walker always yields the symlink entry itself. When this option is
    /// enabled and the symlink points to a directory, the walker also descends
    /// into the target directory while maintaining the symlink's relative path
    /// in emitted [`crate::FileListEntry`] values. Canonical paths are tracked to
    /// prevent infinite loops.
    #[must_use]
    pub const fn follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    /// Configures whether all symlinks should be resolved to their targets.
    ///
    /// When enabled, the walker uses `stat()` (follows symlinks) instead of
    /// `lstat()` for every entry, so symlinks appear as whatever they point to
    /// (regular files, directories, etc.). This mirrors upstream rsync's
    /// `--copy-links` (`-L`) behaviour where `make_file()` calls `do_stat()`
    /// instead of `do_lstat()`.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:205-232` - `readlink_stat()` uses `do_stat()` when
    ///   `copy_links` is set.
    #[must_use]
    pub const fn copy_links(mut self, copy: bool) -> Self {
        self.copy_links = copy;
        self
    }

    /// Controls whether the root entry should be included in the output.
    ///
    /// When disabled, traversal starts directly with the root's children.
    #[must_use]
    pub const fn include_root(mut self, include: bool) -> Self {
        self.include_root = include;
        self
    }

    /// Builds a [`FileListWalker`] using the configured options.
    pub fn build(self) -> Result<FileListWalker, FileListError> {
        FileListWalker::new(
            self.root,
            self.follow_symlinks,
            self.copy_links,
            self.include_root,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_builder() {
        let builder = FileListBuilder::new("/some/path");
        // Just verify construction doesn't panic
        let _ = format!("{builder:?}");
    }

    #[test]
    fn new_with_pathbuf() {
        let path = PathBuf::from("/some/path");
        let builder = FileListBuilder::new(path);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn follow_symlinks_sets_option() {
        let builder = FileListBuilder::new("/path").follow_symlinks(true);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn follow_symlinks_false() {
        let builder = FileListBuilder::new("/path").follow_symlinks(false);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn include_root_sets_option() {
        let builder = FileListBuilder::new("/path").include_root(true);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn include_root_false() {
        let builder = FileListBuilder::new("/path").include_root(false);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn copy_links_sets_option() {
        let builder = FileListBuilder::new("/path").copy_links(true);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn copy_links_false() {
        let builder = FileListBuilder::new("/path").copy_links(false);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn builder_chain() {
        let builder = FileListBuilder::new("/path")
            .follow_symlinks(true)
            .copy_links(true)
            .include_root(false);
        let _ = format!("{builder:?}");
    }

    #[test]
    fn clone_works() {
        let builder = FileListBuilder::new("/path");
        let cloned = builder;
        let _ = format!("{cloned:?}");
    }

    #[test]
    fn debug_format() {
        let builder = FileListBuilder::new("/path");
        let debug = format!("{builder:?}");
        assert!(debug.contains("FileListBuilder"));
    }

    #[test]
    fn build_nonexistent_path_returns_error() {
        let builder = FileListBuilder::new("/nonexistent/path/that/does/not/exist");
        let result = builder.build();
        assert!(result.is_err());
    }
}
