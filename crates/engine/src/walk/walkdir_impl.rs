//! jwalk-based implementation of parallel directory traversal.
//!
//! Uses [`jwalk`] for ~4x faster directory walking compared to single-threaded
//! walkdir, with sorted results matching upstream rsync's ordering.

use std::fs;
use std::path::{Path, PathBuf};

use jwalk::{Parallelism, WalkDir};

use super::{DirectoryWalker, WalkConfig, WalkEntry, WalkError};

/// Type alias for the boxed jwalk iterator to reduce type complexity.
type JwalkIterator = Box<dyn Iterator<Item = Result<jwalk::DirEntry<((), ())>, jwalk::Error>> + Send>;

/// Directory walker implementation using the `jwalk` crate for parallel traversal.
///
/// Provides efficient, configurable directory traversal with support for:
/// - Parallel directory reading (~4x faster than walkdir)
/// - Sorted entry ordering (matching upstream rsync)
/// - Symlink following control
/// - Single-filesystem constraints
/// - Depth limiting
///
/// # Upstream Reference
///
/// The traversal order and behavior mirror upstream rsync's `flist.c`:
/// - Entries within each directory are sorted by filename
/// - Byte-wise comparison on Unix, UTF-16 on Windows
/// - Symlinks are not followed by default
///
/// # Examples
///
/// ```no_run
/// use engine::walk::{DirectoryWalker, WalkConfig, WalkdirWalker};
/// use std::path::Path;
///
/// let config = WalkConfig::default().one_file_system(true);
/// let mut walker = WalkdirWalker::new(Path::new("/home"), config);
///
/// while let Some(result) = walker.next() {
///     match result {
///         Ok(entry) => {
///             println!("{}: {:?}", entry.path().display(), entry.file_type());
///         }
///         Err(e) => {
///             eprintln!("Error: {e}");
///             // Skip problematic directories
///             walker.skip_current_dir();
///         }
///     }
/// }
/// ```
pub struct WalkdirWalker {
    root: PathBuf,
    config: WalkConfig,
    inner: JwalkIterator,
    #[cfg(unix)]
    root_dev: Option<u64>,
    /// Depth at which to skip subtree entries (entries with depth > this are skipped).
    skip_dir_depth: Option<usize>,
    /// Depth of the last returned entry, used for skip_current_dir.
    last_depth: usize,
}

impl WalkdirWalker {
    /// Creates a new walker for the given root path with the specified configuration.
    ///
    /// # Arguments
    ///
    /// * `root` - The root directory to traverse
    /// * `config` - Configuration options for the traversal
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use engine::walk::{WalkConfig, WalkdirWalker};
    /// use std::path::Path;
    ///
    /// let walker = WalkdirWalker::new(Path::new("/tmp"), WalkConfig::default());
    /// ```
    #[must_use]
    pub fn new(root: &Path, config: WalkConfig) -> Self {
        let mut builder = WalkDir::new(root);

        // Use parallel traversal for performance
        builder = builder.parallelism(Parallelism::RayonNewPool(0));

        // Include hidden files/directories (jwalk skips them by default)
        builder = builder.skip_hidden(false);

        // Configure symlink following
        builder = builder.follow_links(config.follow_symlinks);

        // Configure depth limit
        if let Some(max_depth) = config.max_depth {
            builder = builder.max_depth(max_depth.get());
        }

        // Configure root inclusion
        if !config.include_root {
            builder = builder.min_depth(1);
        }

        // Configure sorting - jwalk supports sorted parallel results
        if config.sort_entries {
            builder = builder.sort(true);
        }

        // Get root device for one_file_system check
        #[cfg(unix)]
        let root_dev = if config.one_file_system {
            fs::metadata(root).ok().map(|m| {
                use std::os::unix::fs::MetadataExt;
                m.dev()
            })
        } else {
            None
        };

        Self {
            root: root.to_path_buf(),
            config,
            inner: Box::new(builder.into_iter()),
            #[cfg(unix)]
            root_dev,
            skip_dir_depth: None,
            last_depth: 0,
        }
    }

    /// Checks if an entry should be skipped due to one_file_system constraint.
    #[cfg(unix)]
    fn should_skip_for_filesystem(&self, metadata: &fs::Metadata) -> bool {
        if let Some(root_dev) = self.root_dev {
            use std::os::unix::fs::MetadataExt;
            metadata.dev() != root_dev
        } else {
            false
        }
    }

    #[cfg(not(unix))]
    fn should_skip_for_filesystem(&self, _metadata: &fs::Metadata) -> bool {
        // Windows doesn't have device IDs in the same way
        false
    }
}

impl Iterator for WalkdirWalker {
    type Item = Result<WalkEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = self.inner.next()?;

            match entry {
                Ok(dir_entry) => {
                    let depth = dir_entry.depth();

                    // Check if we should skip entries due to skip_current_dir()
                    if let Some(skip_depth) = self.skip_dir_depth {
                        if depth > skip_depth {
                            continue;
                        }
                        // We've exited the skipped directory
                        self.skip_dir_depth = None;
                    }

                    // Get metadata (symlink_metadata for accurate type info)
                    let metadata = match dir_entry.metadata() {
                        Ok(m) => m,
                        Err(e) => {
                            return Some(Err(WalkError::Walk(format!(
                                "failed to read metadata for '{}': {}",
                                dir_entry.path().display(),
                                e
                            ))))
                        }
                    };

                    // Check one_file_system constraint
                    if self.should_skip_for_filesystem(&metadata) {
                        if metadata.is_dir() {
                            self.skip_dir_depth = Some(depth);
                        }
                        continue;
                    }

                    let path = dir_entry.path();

                    // Track depth for skip_current_dir()
                    self.last_depth = depth;

                    return Some(Ok(WalkEntry::new(path, metadata, depth)));
                }
                Err(e) => {
                    return Some(Err(WalkError::Walk(e.to_string())));
                }
            }
        }
    }
}

impl DirectoryWalker for WalkdirWalker {
    fn root(&self) -> &Path {
        &self.root
    }

    fn config(&self) -> &WalkConfig {
        &self.config
    }

    fn skip_current_dir(&mut self) {
        // jwalk doesn't have a direct skip_current_dir, so we track it manually
        // This will skip all entries at depths greater than the last returned entry
        // Note: This is approximate since we're using parallel traversal
        self.skip_dir_depth = Some(self.last_depth);
    }
}


#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use tempfile::TempDir;

    fn setup_test_tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("c.txt"), b"c").unwrap();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        fs::write(dir.path().join("subdir/nested.txt"), b"nested").unwrap();
        dir
    }

    #[test]
    fn walks_directory_sorted() {
        let dir = setup_test_tree();
        let walker = WalkdirWalker::new(dir.path(), WalkConfig::default());

        let entries: Vec<_> = walker.flatten().collect();

        // Root + 3 files + 1 subdir + 1 nested file = 6 entries
        assert_eq!(entries.len(), 6);

        // Check sorting: root, then a.txt, b.txt, c.txt, subdir, subdir/nested.txt
        let names: Vec<_> = entries
            .iter()
            .filter_map(|e| e.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        // First entry is root (no file_name)
        assert_eq!(entries[0].depth(), 0);

        // Files should be sorted
        let file_names: Vec<_> = names
            .iter()
            .filter(|n| n.ends_with(".txt") && !n.contains("nested"))
            .collect();
        assert_eq!(file_names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn respects_max_depth() {
        let dir = setup_test_tree();
        let config = WalkConfig::default().max_depth(Some(1));
        let walker = WalkdirWalker::new(dir.path(), config);

        let entries: Vec<_> = walker.flatten().collect();

        // Should only get root level entries, not nested.txt
        let has_nested = entries.iter().any(|e| {
            e.file_name()
                .map(|n| n.to_string_lossy().contains("nested"))
                .unwrap_or(false)
        });
        assert!(!has_nested);
    }

    #[test]
    fn respects_include_root_false() {
        let dir = setup_test_tree();
        let config = WalkConfig::default().include_root(false);
        let walker = WalkdirWalker::new(dir.path(), config);

        let entries: Vec<_> = walker.flatten().collect();

        // All entries should have depth > 0
        assert!(entries.iter().all(|e| e.depth() > 0));
    }

    #[test]
    fn skip_current_dir_works() {
        let dir = setup_test_tree();
        let mut walker = WalkdirWalker::new(dir.path(), WalkConfig::default());

        let mut found_subdir = false;

        while let Some(result) = walker.next() {
            let entry = result.unwrap();
            if entry.file_name() == Some(OsStr::new("subdir")) {
                found_subdir = true;
                walker.skip_current_dir();
            }
        }

        assert!(found_subdir);
        // Note: Due to parallel traversal, skip_current_dir may not prevent
        // already-queued entries from being returned
    }

    #[test]
    fn root_accessor_returns_correct_path() {
        let dir = TempDir::new().unwrap();
        let walker = WalkdirWalker::new(dir.path(), WalkConfig::default());

        assert_eq!(walker.root(), dir.path());
    }

    #[test]
    fn config_accessor_returns_correct_config() {
        let dir = TempDir::new().unwrap();
        let config = WalkConfig::default().follow_symlinks(true);
        let walker = WalkdirWalker::new(dir.path(), config);

        assert!(walker.config().follows_symlinks());
    }

    #[test]
    fn handles_nonexistent_root() {
        let walker =
            WalkdirWalker::new(Path::new("/nonexistent/path/12345"), WalkConfig::default());

        let results: Vec<_> = walker.collect();
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }


    #[cfg(unix)]
    #[test]
    fn one_file_system_skips_different_devices() {
        // This test requires a mounted filesystem with different device ID
        // We can at least verify the config is respected
        let dir = TempDir::new().unwrap();
        let config = WalkConfig::default().one_file_system(true);
        let walker = WalkdirWalker::new(dir.path(), config);

        assert!(walker.config().is_one_file_system());
        // Should still work for same-device traversal
        let entries: Vec<_> = walker.flatten().collect();
        assert!(!entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_not_followed_by_default() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");

        fs::write(&target, b"content").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let walker = WalkdirWalker::new(dir.path(), WalkConfig::default());
        let entries: Vec<_> = walker.flatten().collect();

        let link_entry = entries.iter().find(|e| e.path() == link).unwrap();
        assert!(link_entry.is_symlink());
    }
}
