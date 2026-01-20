//! Walkdir-based implementation of directory traversal.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use super::{DirectoryWalker, WalkConfig, WalkEntry, WalkError};

/// Directory walker implementation using the `walkdir` crate.
///
/// Provides efficient, configurable directory traversal with support for:
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
    inner: walkdir::IntoIter,
    #[cfg(unix)]
    root_dev: Option<u64>,
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

        // Configure symlink following
        builder = builder.follow_links(config.follow_symlinks);

        // Configure depth limit
        if let Some(max_depth) = config.max_depth {
            builder = builder.max_depth(max_depth.get());
        }

        // Configure sorting
        if config.sort_entries {
            builder = builder.sort_by(compare_entries);
        }

        // Configure root inclusion
        if !config.include_root {
            builder = builder.min_depth(1);
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
            inner: builder.into_iter(),
            #[cfg(unix)]
            root_dev,
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
                    // Get metadata (symlink_metadata for accurate type info)
                    let metadata = match dir_entry.metadata() {
                        Ok(m) => m,
                        Err(e) => return Some(Err(WalkError::from(e))),
                    };

                    // Check one_file_system constraint
                    if self.should_skip_for_filesystem(&metadata) {
                        if metadata.is_dir() {
                            self.inner.skip_current_dir();
                        }
                        continue;
                    }

                    let path = dir_entry.path().to_path_buf();
                    let depth = dir_entry.depth();

                    return Some(Ok(WalkEntry::new(path, metadata, depth)));
                }
                Err(e) => {
                    return Some(Err(WalkError::from(e)));
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
        self.inner.skip_current_dir();
    }
}

/// Compares directory entries for sorting.
///
/// Uses byte-wise comparison on Unix and UTF-16 comparison on Windows
/// to match upstream rsync's ordering behavior.
fn compare_entries(a: &walkdir::DirEntry, b: &walkdir::DirEntry) -> Ordering {
    compare_file_names(a.file_name(), b.file_name())
}

/// Compares file names using platform-appropriate byte ordering.
///
/// # Platform Behavior
///
/// - **Unix**: Byte-wise comparison of the raw OS string bytes
/// - **Windows**: UTF-16 wide character comparison
/// - **Other**: Lossy UTF-8 string comparison
fn compare_file_names(left: &OsStr, right: &OsStr) -> Ordering {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        left.as_bytes().cmp(right.as_bytes())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        left.encode_wide().cmp(right.encode_wide())
    }

    #[cfg(not(any(unix, windows)))]
    {
        left.to_string_lossy().cmp(&right.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
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
        let mut found_nested = false;

        while let Some(result) = walker.next() {
            let entry = result.unwrap();
            if entry.file_name() == Some(OsStr::new("subdir")) {
                found_subdir = true;
                walker.skip_current_dir();
            }
            if entry.file_name() == Some(OsStr::new("nested.txt")) {
                found_nested = true;
            }
        }

        assert!(found_subdir);
        assert!(!found_nested);
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

    #[test]
    fn compare_file_names_ordering() {
        assert_eq!(
            compare_file_names(OsStr::new("a"), OsStr::new("b")),
            Ordering::Less
        );
        assert_eq!(
            compare_file_names(OsStr::new("b"), OsStr::new("a")),
            Ordering::Greater
        );
        assert_eq!(
            compare_file_names(OsStr::new("a"), OsStr::new("a")),
            Ordering::Equal
        );
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
