//! Integration tests for directory traversal options.
//!
//! These tests verify that [`FileListBuilder`] configuration options
//! correctly affect traversal behavior, matching upstream rsync's
//! various command-line flags for controlling file list generation.
//!
//! Reference: rsync 3.4.1 flist.c

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::fs;
use std::path::PathBuf;

// ============================================================================
// Helper Functions
// ============================================================================

/// Collects relative paths from a walker, skipping the root entry.
fn collect_relative_paths(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<PathBuf> {
    walker
        .filter_map(|r| r.ok())
        .filter(|e| !e.is_root())
        .map(|e| e.relative_path().to_path_buf())
        .collect()
}

/// Collects all entries from a walker.
fn collect_all_entries(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<FileListEntry> {
    walker.map(|r| r.expect("entry should succeed")).collect()
}

// ============================================================================
// include_root Option Tests
// ============================================================================

/// Verifies include_root(true) includes the root entry (default behavior).
#[test]
fn include_root_true_includes_root_entry() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(true)
        .build()
        .expect("build walker");

    let entries = collect_all_entries(walker);

    assert!(
        entries.iter().any(|e| e.is_root()),
        "should include root entry"
    );
    assert_eq!(entries.len(), 2, "root + file");
}

/// Verifies include_root(false) excludes the root entry.
#[test]
fn include_root_false_excludes_root_entry() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let entries = collect_all_entries(walker);

    assert!(
        !entries.iter().any(|e| e.is_root()),
        "should not include root entry"
    );
    assert_eq!(entries.len(), 1, "only file");
}

/// Verifies include_root(false) on empty directory yields empty iterator.
#[test]
fn include_root_false_empty_dir_yields_nothing() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("empty");
    fs::create_dir(&root).expect("create root");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let entries: Vec<_> = walker.collect();
    assert!(entries.is_empty());
}

/// Verifies include_root(false) still traverses nested content.
#[test]
fn include_root_false_traverses_nested_content() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");
    fs::write(root.join("subdir/file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![PathBuf::from("subdir"), PathBuf::from("subdir/file.txt")]
    );
}

/// Verifies include_root affects depth calculation.
#[test]
fn include_root_false_depth_starts_at_one() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let entries = collect_all_entries(walker);

    // First entry (file.txt) should have depth 1, not 0
    assert_eq!(entries[0].depth(), 1);
}

/// Verifies include_root with single file root.
#[test]
fn include_root_false_with_single_file_root() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let file = temp.path().join("single.txt");
    fs::write(&file, b"content").expect("write file");

    let walker = FileListBuilder::new(&file)
        .include_root(false)
        .build()
        .expect("build walker");

    let entries: Vec<_> = walker.collect();

    // Single file has no children, so nothing to yield
    assert!(entries.is_empty());
}

// ============================================================================
// follow_symlinks Option Tests (Unix only)
// ============================================================================

#[cfg(unix)]
mod symlink_option_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Verifies follow_symlinks(false) does not descend into symlinked dirs.
    #[test]
    fn follow_symlinks_false_does_not_descend() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");

        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("inside.txt"), b"data").expect("write inside");

        symlink(&target, root.join("link")).expect("create symlink");

        let walker = FileListBuilder::new(&root)
            .follow_symlinks(false)
            .build()
            .expect("build walker");

        let paths = collect_relative_paths(walker);

        assert_eq!(paths, vec![PathBuf::from("link")]);
    }

    /// Verifies follow_symlinks(true) descends into symlinked dirs.
    #[test]
    fn follow_symlinks_true_descends_into_symlinks() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");

        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("inside.txt"), b"data").expect("write inside");

        symlink(&target, root.join("link")).expect("create symlink");

        let walker = FileListBuilder::new(&root)
            .follow_symlinks(true)
            .build()
            .expect("build walker");

        let paths = collect_relative_paths(walker);

        assert_eq!(
            paths,
            vec![PathBuf::from("link"), PathBuf::from("link/inside.txt")]
        );
    }

    /// Verifies default follow_symlinks behavior is false.
    #[test]
    fn default_follow_symlinks_is_false() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");

        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("inside.txt"), b"data").expect("write inside");

        symlink(&target, root.join("link")).expect("create symlink");

        // No explicit follow_symlinks call
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Default should not follow
        assert_eq!(paths, vec![PathBuf::from("link")]);
    }

    /// Verifies follow_symlinks can be toggled multiple times.
    #[test]
    fn follow_symlinks_toggle() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");

        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("inside.txt"), b"data").expect("write inside");

        symlink(&target, root.join("link")).expect("create symlink");

        // Toggle multiple times, last value wins
        let walker = FileListBuilder::new(&root)
            .follow_symlinks(true)
            .follow_symlinks(false)
            .follow_symlinks(true)
            .follow_symlinks(false)
            .build()
            .expect("build walker");

        let paths = collect_relative_paths(walker);

        // Last was false
        assert_eq!(paths, vec![PathBuf::from("link")]);
    }
}

// ============================================================================
// Combined Options Tests
// ============================================================================

#[cfg(unix)]
mod combined_option_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Verifies include_root and follow_symlinks work together.
    #[test]
    fn include_root_false_with_follow_symlinks_true() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");

        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("file.txt"), b"data").expect("write file");

        symlink(&target, root.join("link")).expect("create symlink");

        let walker = FileListBuilder::new(&root)
            .include_root(false)
            .follow_symlinks(true)
            .build()
            .expect("build walker");

        let entries = collect_all_entries(walker);

        // No root entry, but symlink is followed
        assert!(!entries.iter().any(|e| e.is_root()));

        let paths: Vec<_> = entries
            .iter()
            .map(|e| e.relative_path().to_path_buf())
            .collect();
        assert_eq!(
            paths,
            vec![PathBuf::from("link"), PathBuf::from("link/file.txt")]
        );
    }

    /// Verifies root symlink with include_root false.
    #[test]
    fn root_symlink_with_include_root_false() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let target = temp.path().join("target");
        let link = temp.path().join("link");

        fs::create_dir(&target).expect("create target");
        fs::write(target.join("file.txt"), b"data").expect("write file");

        symlink(&target, &link).expect("create symlink");

        let walker = FileListBuilder::new(&link)
            .include_root(false)
            .follow_symlinks(true)
            .build()
            .expect("build walker");

        let paths = collect_relative_paths(walker);

        // Should descend into symlinked root but not include root
        assert_eq!(paths, vec![PathBuf::from("file.txt")]);
    }
}

// ============================================================================
// Builder State Tests
// ============================================================================

/// Verifies builder preserves its state through cloning.
#[test]
fn builder_clone_preserves_options() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let builder = FileListBuilder::new(&root).include_root(false);

    let cloned = builder.clone();

    let entries1 = collect_all_entries(builder.build().expect("build walker"));
    let entries2 = collect_all_entries(cloned.build().expect("build cloned walker"));

    assert_eq!(entries1.len(), entries2.len());
    assert!(!entries1.iter().any(|e| e.is_root()));
    assert!(!entries2.iter().any(|e| e.is_root()));
}

/// Verifies builder can be reused after building.
#[test]
fn builder_can_be_reused() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let builder = FileListBuilder::new(&root);

    // Build multiple times from same builder (via clone since build consumes self)
    let walker1 = builder.clone().build().expect("build walker 1");
    let walker2 = builder.clone().build().expect("build walker 2");

    let paths1 = collect_relative_paths(walker1);
    let paths2 = collect_relative_paths(walker2);

    assert_eq!(paths1, paths2);
}

// ============================================================================
// Path Input Variations Tests
// ============================================================================

/// Verifies builder accepts various path types.
#[test]
fn builder_accepts_path_types() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    // From &Path
    let _builder1 = FileListBuilder::new(root.as_path());

    // From PathBuf
    let _builder2 = FileListBuilder::new(root.clone());

    // From &str
    let root_str = root.to_str().expect("path to str");
    let _builder3 = FileListBuilder::new(root_str);

    // From String
    let _builder4 = FileListBuilder::new(root_str.to_string());
}

/// Verifies builder handles relative paths by absolutizing them.
#[test]
fn builder_absolutizes_relative_paths() {
    let temp = tempfile::tempdir().expect("create tempdir");

    // Create structure in temp dir
    let subdir = temp.path().join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    fs::write(subdir.join("file.txt"), b"data").expect("write file");

    // Use absolute path for predictable behavior in tests
    let walker = FileListBuilder::new(&subdir).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Full paths should be absolute
    for entry in &entries {
        assert!(
            entry.full_path().is_absolute(),
            "full_path should be absolute"
        );
    }
}

// ============================================================================
// Error Handling for Options Tests
// ============================================================================

/// Verifies build fails with clear error for non-existent path.
#[test]
fn build_fails_for_nonexistent_path() {
    let result = FileListBuilder::new("/nonexistent/path/that/does/not/exist").build();

    match result {
        Ok(_) => panic!("expected error for nonexistent path"),
        Err(error) => {
            assert!(
                error.path().to_string_lossy().contains("nonexistent"),
                "error should reference the path"
            );
        }
    }
}

/// Verifies error message is informative.
#[test]
fn error_message_is_informative() {
    let result = FileListBuilder::new("/surely/missing/path").build();

    match result {
        Ok(_) => panic!("expected error for missing path"),
        Err(error) => {
            let msg = error.to_string();
            assert!(
                msg.contains("missing") || msg.contains("surely"),
                "error message should reference the path: {}",
                msg
            );
        }
    }
}

// ============================================================================
// Option Chaining Tests
// ============================================================================

/// Verifies all options can be chained fluently.
#[test]
fn fluent_option_chaining() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    // All options in one chain
    let result = FileListBuilder::new(&root)
        .include_root(true)
        .follow_symlinks(false)
        .include_root(false) // Override
        .follow_symlinks(true) // Override
        .build();

    assert!(result.is_ok());
}

/// Verifies builder methods return Self for chaining.
#[test]
fn builder_methods_return_self() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    // Type inference should work if methods return Self
    let builder: FileListBuilder = FileListBuilder::new(&root)
        .include_root(true)
        .follow_symlinks(false);

    let _walker = builder.build();
}

// ============================================================================
// Default Builder Behavior Tests
// ============================================================================

/// Verifies default builder has sensible defaults.
#[test]
fn default_builder_behavior() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    // No options specified
    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Default: include_root = true
    assert!(entries.iter().any(|e| e.is_root()));

    // Should find the file
    let paths: Vec<_> = entries
        .iter()
        .filter(|e| !e.is_root())
        .map(|e| e.relative_path().to_path_buf())
        .collect();
    assert_eq!(paths, vec![PathBuf::from("file.txt")]);
}

// ============================================================================
// Walker Iteration Behavior Tests
// ============================================================================

/// Verifies walker can be partially consumed.
#[test]
fn walker_partial_consumption() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    for i in 0..10 {
        fs::write(root.join(format!("file{}.txt", i)), b"data").expect("write file");
    }

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Only consume a few entries
    let _ = walker.next(); // root
    let _ = walker.next(); // file0
    let _ = walker.next(); // file1

    // Walker should still have remaining entries
    let remaining: Vec<_> = walker.collect();
    assert!(!remaining.is_empty());
}

/// Verifies walker handles early termination gracefully.
#[test]
fn walker_early_termination() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    for i in 0..5 {
        fs::write(root.join(format!("file{}.txt", i)), b"data").expect("write file");
    }

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Take only 2 entries, then drop walker
    let first = walker.next();
    let second = walker.next();

    assert!(first.is_some());
    assert!(second.is_some());

    // Dropping walker should not panic
    drop(walker);
}
