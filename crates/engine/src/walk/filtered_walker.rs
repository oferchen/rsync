//! Filtered directory traversal with early pruning.
//!
//! This module provides [`FilteredWalker`], a decorator that applies filter
//! rules during directory traversal, enabling early pruning of excluded
//! directories to avoid unnecessary I/O.

use std::path::Path;

use filters::FilterSet;

use super::{DirectoryWalker, WalkConfig, WalkEntry, WalkError};

/// A walker decorator that applies filter rules during traversal.
///
/// `FilteredWalker` wraps any [`DirectoryWalker`] implementation and applies
/// a [`FilterSet`] to each entry. When a directory is excluded by the filter
/// rules, the walker calls `skip_current_dir()` on the inner walker to avoid
/// traversing its contents, providing early pruning for improved performance.
///
/// # Upstream Reference
///
/// This matches upstream rsync's behavior where filter rules are evaluated
/// during directory traversal in `flist.c`, and excluded directories have
/// their contents skipped entirely rather than being traversed and filtered
/// entry-by-entry.
///
/// # Examples
///
/// ```no_run
/// use engine::walk::{FilteredWalker, WalkConfig, WalkdirWalker};
/// use filters::{FilterRule, FilterSet};
/// use std::path::Path;
///
/// let rules = vec![
///     FilterRule::exclude(".git/".to_owned()),
///     FilterRule::exclude("target/".to_owned()),
/// ];
/// let filters = FilterSet::from_rules(rules).unwrap();
///
/// let inner = WalkdirWalker::new(Path::new("/project"), WalkConfig::default());
/// let walker = FilteredWalker::new(inner, filters);
///
/// for entry in walker.flatten() {
///     println!("{}", entry.path().display());
/// }
/// ```
pub struct FilteredWalker<W> {
    inner: W,
    filters: FilterSet,
}

impl<W: DirectoryWalker> FilteredWalker<W> {
    /// Creates a new filtered walker wrapping the given walker with filter rules.
    ///
    /// # Arguments
    ///
    /// * `inner` - The underlying directory walker to filter
    /// * `filters` - The filter set to apply to each entry
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use engine::walk::{FilteredWalker, WalkConfig, WalkdirWalker};
    /// use filters::FilterSet;
    /// use std::path::Path;
    ///
    /// let inner = WalkdirWalker::new(Path::new("/src"), WalkConfig::default());
    /// let filters = FilterSet::default();
    /// let walker = FilteredWalker::new(inner, filters);
    /// ```
    #[must_use]
    pub fn new(inner: W, filters: FilterSet) -> Self {
        Self { inner, filters }
    }

    /// Returns a reference to the filter set.
    #[must_use]
    pub fn filters(&self) -> &FilterSet {
        &self.filters
    }

    /// Consumes the filtered walker and returns the inner walker.
    #[must_use]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: DirectoryWalker> Iterator for FilteredWalker<W> {
    type Item = Result<WalkEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let result = self.inner.next()?;

            match result {
                Ok(entry) => {
                    // Get the relative path for filter matching
                    let rel_path = match entry.path().strip_prefix(self.inner.root()) {
                        Ok(p) => p,
                        Err(_) => {
                            // Root entry or path outside root - always yield it
                            return Some(Ok(entry));
                        }
                    };

                    // Skip empty relative paths (root)
                    if rel_path.as_os_str().is_empty() {
                        return Some(Ok(entry));
                    }

                    let is_dir = entry.is_dir();

                    // Check if the entry is allowed by the filter rules
                    if self.filters.allows(rel_path, is_dir) {
                        return Some(Ok(entry));
                    }

                    // Entry is excluded
                    if is_dir {
                        // For directories, skip the entire subtree
                        self.inner.skip_current_dir();
                    }
                    // Continue to next entry (skip this one)
                }
                Err(e) => {
                    // Propagate errors
                    return Some(Err(e));
                }
            }
        }
    }
}

impl<W: DirectoryWalker> DirectoryWalker for FilteredWalker<W> {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn config(&self) -> &WalkConfig {
        self.inner.config()
    }

    fn skip_current_dir(&mut self) {
        self.inner.skip_current_dir();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk::WalkdirWalker;
    use filters::FilterRule;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file.txt"), b"content").unwrap();
        fs::write(dir.path().join("file.bak"), b"backup").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        fs::write(dir.path().join("subdir/nested.txt"), b"nested").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), b"git config").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), b"fn main() {}").unwrap();
        dir
    }

    #[test]
    fn empty_filter_yields_all_entries() {
        let dir = setup_test_tree();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let filters = FilterSet::default();
        let walker = FilteredWalker::new(inner, filters);

        let entries: Vec<_> = walker.flatten().collect();
        // Should have all entries: root + file.txt + file.bak + subdir + nested.txt + .git + config + src + main.rs
        assert_eq!(entries.len(), 9);
    }

    #[test]
    fn excludes_files_by_pattern() {
        let dir = setup_test_tree();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let rules = vec![FilterRule::exclude("*.bak".to_owned())];
        let filters = FilterSet::from_rules(rules).unwrap();
        let walker = FilteredWalker::new(inner, filters);

        let entries: Vec<_> = walker.flatten().collect();
        let has_bak = entries.iter().any(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "bak")
                .unwrap_or(false)
        });
        assert!(!has_bak);
    }

    #[test]
    fn excludes_directories_and_prunes_subtree() {
        let dir = setup_test_tree();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let rules = vec![FilterRule::exclude(".git/".to_owned())];
        let filters = FilterSet::from_rules(rules).unwrap();
        let walker = FilteredWalker::new(inner, filters);

        let entries: Vec<_> = walker.flatten().collect();

        // Should not have .git directory or its contents
        let has_git = entries.iter().any(|e| {
            e.path()
                .components()
                .any(|c| c.as_os_str() == ".git")
        });
        assert!(!has_git);
    }

    #[test]
    fn include_overrides_exclude() {
        let dir = setup_test_tree();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        // Exclude all .txt files except those in src/
        let rules = vec![
            FilterRule::exclude("*.txt".to_owned()),
            FilterRule::include("src/**".to_owned()),
        ];
        let filters = FilterSet::from_rules(rules).unwrap();
        let walker = FilteredWalker::new(inner, filters);

        let entries: Vec<_> = walker.flatten().collect();

        // file.txt and subdir/nested.txt should be excluded
        let has_root_txt = entries.iter().any(|e| e.path().ends_with("file.txt"));
        assert!(!has_root_txt);

        // src/ and src/main.rs should be included
        let has_src = entries.iter().any(|e| {
            e.path()
                .file_name()
                .map(|n| n == "src")
                .unwrap_or(false)
        });
        assert!(has_src);
    }

    #[test]
    fn filters_accessor_returns_filter_set() {
        let dir = TempDir::new().unwrap();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let rules = vec![FilterRule::exclude("*.tmp".to_owned())];
        let filters = FilterSet::from_rules(rules).unwrap();
        let walker = FilteredWalker::new(inner, filters);

        assert!(!walker.filters().is_empty());
    }

    #[test]
    fn into_inner_returns_walker() {
        let dir = TempDir::new().unwrap();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let filters = FilterSet::default();
        let walker = FilteredWalker::new(inner, filters);

        let recovered = walker.into_inner();
        assert_eq!(recovered.root(), dir.path());
    }

    #[test]
    fn root_returns_inner_root() {
        let dir = TempDir::new().unwrap();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let filters = FilterSet::default();
        let walker = FilteredWalker::new(inner, filters);

        assert_eq!(walker.root(), dir.path());
    }

    #[test]
    fn config_returns_inner_config() {
        let dir = TempDir::new().unwrap();
        let config = WalkConfig::default().follow_symlinks(true);
        let inner = WalkdirWalker::new(dir.path(), config);
        let filters = FilterSet::default();
        let walker = FilteredWalker::new(inner, filters);

        assert!(walker.config().follows_symlinks());
    }

    #[test]
    fn skip_current_dir_delegates_to_inner() {
        let dir = setup_test_tree();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let filters = FilterSet::default();
        let mut walker = FilteredWalker::new(inner, filters);

        let mut found_subdir = false;
        let mut found_nested = false;

        while let Some(result) = walker.next() {
            let entry = result.unwrap();
            if entry.file_name().map(|n| n == "subdir").unwrap_or(false) {
                found_subdir = true;
                walker.skip_current_dir();
            }
            if entry.file_name().map(|n| n == "nested.txt").unwrap_or(false) {
                found_nested = true;
            }
        }

        assert!(found_subdir);
        assert!(!found_nested);
    }

    #[test]
    fn handles_errors_from_inner_walker() {
        // Test that errors from the inner walker are propagated
        let walker = WalkdirWalker::new(
            Path::new("/nonexistent/path/12345"),
            WalkConfig::default(),
        );
        let filters = FilterSet::default();
        let filtered = FilteredWalker::new(walker, filters);

        let results: Vec<_> = filtered.collect();
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }

    #[test]
    fn multiple_exclude_patterns() {
        let dir = setup_test_tree();
        let inner = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let rules = vec![
            FilterRule::exclude("*.bak".to_owned()),
            FilterRule::exclude(".git/".to_owned()),
            FilterRule::exclude("subdir/".to_owned()),
        ];
        let filters = FilterSet::from_rules(rules).unwrap();
        let walker = FilteredWalker::new(inner, filters);

        let entries: Vec<_> = walker.flatten().collect();

        // Should only have: root, file.txt, src/, src/main.rs
        let names: Vec<_> = entries
            .iter()
            .filter_map(|e| e.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        assert!(!names.contains(&"file.bak".to_string()));
        assert!(!names.contains(&".git".to_string()));
        assert!(!names.contains(&"subdir".to_string()));
        assert!(names.contains(&"file.txt".to_string()));
        assert!(names.contains(&"src".to_string()));
    }
}
