use crate::error::WalkError;
use crate::walker::Walker;
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
pub struct WalkBuilder {
    root: PathBuf,
    follow_symlinks: bool,
    include_root: bool,
}

impl WalkBuilder {
    /// Creates a new builder that will traverse the provided root path.
    #[must_use]
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            follow_symlinks: false,
            include_root: true,
        }
    }

    /// Configures whether directory symlinks should be traversed.
    ///
    /// The walker always yields the symlink entry itself. When this option is
    /// enabled and the symlink points to a directory, the walker also descends
    /// into the target directory while maintaining the symlink's relative path
    /// in emitted [`crate::WalkEntry`] values. Canonical paths are tracked to
    /// prevent infinite loops.
    #[must_use]
    pub const fn follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
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

    /// Builds a [`Walker`] using the configured options.
    pub fn build(self) -> Result<Walker, WalkError> {
        Walker::new(self.root, self.follow_symlinks, self.include_root)
    }
}
