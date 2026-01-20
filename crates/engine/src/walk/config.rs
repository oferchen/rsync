//! Configuration for directory traversal.

use std::num::NonZeroUsize;

/// Configuration options for directory traversal.
///
/// This struct uses the builder pattern to configure how directories are
/// traversed. All options have sensible defaults matching upstream rsync's
/// default behavior.
///
/// # Upstream Reference
///
/// Configuration maps to upstream rsync flags:
/// - `follow_symlinks` → `-L` / `--copy-links`
/// - `one_file_system` → `-x` / `--one-file-system`
/// - `max_depth` → `--max-depth=N` (rsync 3.1.0+)
///
/// # Examples
///
/// ```
/// use engine::walk::WalkConfig;
///
/// // Default configuration (no symlink following, all filesystems)
/// let config = WalkConfig::default();
///
/// // Configure for single-filesystem traversal with depth limit
/// let config = WalkConfig::default()
///     .one_file_system(true)
///     .max_depth(Some(10));
/// ```
#[derive(Clone, Debug)]
pub struct WalkConfig {
    /// Follow symbolic links during traversal.
    ///
    /// When `true`, symlinks are dereferenced and their targets are
    /// traversed. When `false` (default), symlinks are yielded as-is
    /// without following.
    pub(crate) follow_symlinks: bool,

    /// Restrict traversal to a single filesystem.
    ///
    /// When `true`, directories on different filesystems than the root
    /// are not descended into. This matches upstream rsync's `-x` flag.
    pub(crate) one_file_system: bool,

    /// Maximum depth to descend into directories.
    ///
    /// `None` means unlimited depth. `Some(0)` yields only the root.
    /// `Some(1)` yields the root and its immediate children, etc.
    pub(crate) max_depth: Option<NonZeroUsize>,

    /// Sort entries within each directory.
    ///
    /// When `true` (default), entries are sorted using byte-wise
    /// comparison on Unix and UTF-16 comparison on Windows, matching
    /// upstream rsync's ordering.
    pub(crate) sort_entries: bool,

    /// Yield the root directory itself as the first entry.
    ///
    /// When `true` (default), the root path is yielded before its
    /// contents. When `false`, only the root's contents are yielded.
    pub(crate) include_root: bool,
}

impl Default for WalkConfig {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            one_file_system: false,
            max_depth: None,
            sort_entries: true,
            include_root: true,
        }
    }
}

impl WalkConfig {
    /// Creates a new configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets whether to follow symbolic links.
    ///
    /// # Upstream Reference
    ///
    /// Maps to `-L` / `--copy-links` flag.
    #[must_use]
    pub const fn follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    /// Sets whether to restrict traversal to a single filesystem.
    ///
    /// # Upstream Reference
    ///
    /// Maps to `-x` / `--one-file-system` flag.
    #[must_use]
    pub const fn one_file_system(mut self, single_fs: bool) -> Self {
        self.one_file_system = single_fs;
        self
    }

    /// Sets the maximum depth to descend.
    ///
    /// `None` means unlimited depth. `Some(n)` limits descent to `n` levels
    /// below the root.
    ///
    /// # Upstream Reference
    ///
    /// Maps to `--max-depth=N` (rsync 3.1.0+).
    #[must_use]
    pub const fn max_depth(mut self, depth: Option<usize>) -> Self {
        self.max_depth = match depth {
            Some(d) => NonZeroUsize::new(d),
            None => None,
        };
        self
    }

    /// Sets whether to sort entries within each directory.
    ///
    /// Sorting ensures deterministic output matching upstream rsync's
    /// file list ordering.
    #[must_use]
    pub const fn sort_entries(mut self, sort: bool) -> Self {
        self.sort_entries = sort;
        self
    }

    /// Sets whether to include the root directory as the first entry.
    #[must_use]
    pub const fn include_root(mut self, include: bool) -> Self {
        self.include_root = include;
        self
    }

    /// Returns whether symlinks are followed.
    #[must_use]
    pub const fn follows_symlinks(&self) -> bool {
        self.follow_symlinks
    }

    /// Returns whether traversal is restricted to one filesystem.
    #[must_use]
    pub const fn is_one_file_system(&self) -> bool {
        self.one_file_system
    }

    /// Returns the maximum depth, if set.
    #[must_use]
    pub const fn get_max_depth(&self) -> Option<NonZeroUsize> {
        self.max_depth
    }

    /// Returns whether entries are sorted.
    #[must_use]
    pub const fn sorts_entries(&self) -> bool {
        self.sort_entries
    }

    /// Returns whether the root is included as the first entry.
    #[must_use]
    pub const fn includes_root(&self) -> bool {
        self.include_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let config = WalkConfig::default();
        assert!(!config.follows_symlinks());
        assert!(!config.is_one_file_system());
        assert!(config.get_max_depth().is_none());
        assert!(config.sorts_entries());
        assert!(config.includes_root());
    }

    #[test]
    fn builder_methods_set_values() {
        let config = WalkConfig::new()
            .follow_symlinks(true)
            .one_file_system(true)
            .max_depth(Some(5))
            .sort_entries(false)
            .include_root(false);

        assert!(config.follows_symlinks());
        assert!(config.is_one_file_system());
        assert_eq!(config.get_max_depth().map(|n| n.get()), Some(5));
        assert!(!config.sorts_entries());
        assert!(!config.includes_root());
    }

    #[test]
    fn max_depth_zero_becomes_none() {
        let config = WalkConfig::new().max_depth(Some(0));
        assert!(config.get_max_depth().is_none());
    }

    #[test]
    fn config_is_clone() {
        let config = WalkConfig::new().follow_symlinks(true);
        let cloned = config.clone();
        assert!(cloned.follows_symlinks());
    }

    #[test]
    fn config_is_debug() {
        let config = WalkConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("WalkConfig"));
    }
}
