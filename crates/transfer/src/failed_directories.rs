//! Failed directory tracking for incremental file list processing.
//!
//! Provides efficient tracking of directory creation failures to avoid
//! redundant operations on files within failed directory trees.

use std::collections::HashSet;

/// Tracks directories that failed to create during incremental processing.
///
/// Children of failed directories are skipped during incremental processing
/// to avoid cascading failures and unnecessary operations.
///
/// # Example
///
/// ```
/// use transfer::FailedDirectories;
///
/// let mut failed = FailedDirectories::new();
/// failed.mark_failed("foo/bar");
///
/// // Child paths are detected
/// assert!(failed.failed_ancestor("foo/bar/baz/file.txt").is_some());
///
/// // Sibling paths are not affected
/// assert!(failed.failed_ancestor("foo/other/file.txt").is_none());
/// ```
#[derive(Debug, Default, Clone)]
pub struct FailedDirectories {
    /// Failed directory paths (normalized, no trailing slash).
    paths: HashSet<String>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    ///
    /// The path should be normalized without a trailing slash.
    #[inline]
    pub fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    /// Checks if an entry path has a failed ancestor directory.
    ///
    /// Returns the failed ancestor path if found, `None` otherwise.
    /// This performs efficient prefix matching by walking up the path tree.
    ///
    /// # Example
    ///
    /// ```
    /// use transfer::FailedDirectories;
    ///
    /// let mut failed = FailedDirectories::new();
    /// failed.mark_failed("a/b");
    ///
    /// assert_eq!(failed.failed_ancestor("a/b/c/file.txt"), Some("a/b"));
    /// assert_eq!(failed.failed_ancestor("a/c/file.txt"), None);
    /// ```
    pub fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        // Check if exact path is failed
        if self.paths.contains(entry_path) {
            return self.paths.get(entry_path).map(|s| s.as_str());
        }

        // Check each parent path component by walking backwards
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if let Some(failed) = self.paths.get(check_path) {
                return Some(failed.as_str());
            }
        }
        None
    }

    /// Returns the number of failed directories tracked.
    #[inline]
    pub fn count(&self) -> usize {
        self.paths.len()
    }

    /// Clears all tracked failed directories.
    #[inline]
    pub fn clear(&mut self) {
        self.paths.clear();
    }

    /// Returns true if no directories have failed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_is_empty() {
        let failed = FailedDirectories::new();
        assert_eq!(failed.count(), 0);
        assert!(failed.is_empty());
        assert!(failed.failed_ancestor("any/path").is_none());
    }

    #[test]
    fn marks_and_finds_exact_match() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");

        assert_eq!(failed.count(), 1);
        assert!(!failed.is_empty());
        assert_eq!(failed.failed_ancestor("foo/bar"), Some("foo/bar"));
    }

    #[test]
    fn finds_direct_child() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");

        assert_eq!(
            failed.failed_ancestor("foo/bar/file.txt"),
            Some("foo/bar")
        );
    }

    #[test]
    fn finds_nested_descendant() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");

        assert_eq!(
            failed.failed_ancestor("foo/bar/baz/deep/file.txt"),
            Some("foo/bar")
        );
    }

    #[test]
    fn does_not_match_sibling() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");

        assert!(failed.failed_ancestor("foo/other/file.txt").is_none());
        assert!(failed.failed_ancestor("foo/bar2/file.txt").is_none());
    }

    #[test]
    fn does_not_match_parent() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");

        assert!(failed.failed_ancestor("foo/file.txt").is_none());
        assert!(failed.failed_ancestor("foo").is_none());
    }

    #[test]
    fn handles_root_level_directory() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("toplevel");

        assert_eq!(
            failed.failed_ancestor("toplevel/sub/file.txt"),
            Some("toplevel")
        );
        assert!(failed.failed_ancestor("other/file.txt").is_none());
    }

    #[test]
    fn counts_multiple_failures() {
        let mut failed = FailedDirectories::new();

        failed.mark_failed("a");
        assert_eq!(failed.count(), 1);

        failed.mark_failed("b");
        assert_eq!(failed.count(), 2);

        failed.mark_failed("c/d");
        assert_eq!(failed.count(), 3);
    }

    #[test]
    fn duplicate_marks_do_not_increase_count() {
        let mut failed = FailedDirectories::new();

        failed.mark_failed("foo/bar");
        assert_eq!(failed.count(), 1);

        failed.mark_failed("foo/bar");
        assert_eq!(failed.count(), 1);
    }

    #[test]
    fn clear_removes_all_failures() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a");
        failed.mark_failed("b");
        assert_eq!(failed.count(), 2);

        failed.clear();
        assert_eq!(failed.count(), 0);
        assert!(failed.is_empty());
        assert!(failed.failed_ancestor("a/file.txt").is_none());
    }

    #[test]
    fn handles_deeply_nested_hierarchy() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("level1");

        assert!(failed.failed_ancestor("level1/level2").is_some());
        assert!(failed.failed_ancestor("level1/level2/level3").is_some());
        assert!(failed.failed_ancestor("level1/level2/level3/level4/file.txt").is_some());
    }

    #[test]
    fn multiple_failed_dirs_independent() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a/b");
        failed.mark_failed("x/y");

        assert_eq!(failed.failed_ancestor("a/b/c/file.txt"), Some("a/b"));
        assert_eq!(failed.failed_ancestor("x/y/z/file.txt"), Some("x/y"));
        assert!(failed.failed_ancestor("a/c/file.txt").is_none());
        assert!(failed.failed_ancestor("x/z/file.txt").is_none());
    }

    #[test]
    fn empty_path_returns_none() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo");

        assert!(failed.failed_ancestor("").is_none());
    }

    #[test]
    fn path_without_slashes() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("file");

        assert_eq!(failed.failed_ancestor("file"), Some("file"));
        assert!(failed.failed_ancestor("other").is_none());
    }

    #[test]
    fn clone_creates_independent_copy() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo");

        let mut cloned = failed.clone();
        cloned.mark_failed("bar");

        assert_eq!(failed.count(), 1);
        assert_eq!(cloned.count(), 2);
        assert!(failed.failed_ancestor("bar/file.txt").is_none());
        assert!(cloned.failed_ancestor("bar/file.txt").is_some());
    }

    #[test]
    fn prefix_matching_not_confused_by_similar_names() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("abc");

        // "abcd" contains "abc" but is not a child path
        assert!(failed.failed_ancestor("abcd").is_none());
        assert!(failed.failed_ancestor("abcd/file.txt").is_none());

        // Only actual children should match
        assert!(failed.failed_ancestor("abc/file.txt").is_some());
    }

    #[test]
    fn returns_closest_failed_ancestor() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a");
        failed.mark_failed("a/b");
        failed.mark_failed("a/b/c");

        // Should return the closest (most specific) failed ancestor
        let result = failed.failed_ancestor("a/b/c/d/file.txt");
        assert!(result.is_some());
        // The implementation returns the first match found walking up,
        // which is "a/b/c" in this case
        assert_eq!(result, Some("a/b/c"));
    }

    #[test]
    fn default_is_empty() {
        let failed = FailedDirectories::default();
        assert!(failed.is_empty());
        assert_eq!(failed.count(), 0);
    }
}
