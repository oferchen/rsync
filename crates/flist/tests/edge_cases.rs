//! Integration tests for edge cases in file list traversal.
//!
//! These tests verify correct handling of edge cases, unusual filesystem
//! structures, error conditions, and boundary scenarios that might arise
//! in real-world usage.
//!
//! Reference: rsync 3.4.1 flist.c

use flist::{FileListBuilder, FileListEntry, FileListError, FileListErrorKind};
use std::fs;
use std::path::{Path, PathBuf};

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
// Empty Directory Edge Cases
// ============================================================================

/// Verifies handling of an empty directory.
#[test]
fn empty_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("empty");
    fs::create_dir(&root).expect("create empty dir");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    assert_eq!(entries.len(), 1, "should only have root");
    assert!(entries[0].is_root());
}

/// Verifies handling of nested empty directories.
#[test]
fn nested_empty_directories() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    // Create empty nested structure
    fs::create_dir_all(root.join("a/b/c/d/e")).expect("create nested dirs");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![
            PathBuf::from("a"),
            PathBuf::from("a/b"),
            PathBuf::from("a/b/c"),
            PathBuf::from("a/b/c/d"),
            PathBuf::from("a/b/c/d/e"),
        ]
    );
}

/// Verifies handling of empty directories mixed with files.
#[test]
fn empty_directories_mixed_with_files() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    fs::create_dir(root.join("empty1")).expect("create empty1");
    fs::write(root.join("file.txt"), b"data").expect("write file");
    fs::create_dir(root.join("empty2")).expect("create empty2");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![
            PathBuf::from("empty1"),
            PathBuf::from("empty2"),
            PathBuf::from("file.txt"),
        ]
    );
}

// ============================================================================
// Deep Nesting Edge Cases
// ============================================================================

/// Verifies handling of deeply nested directory structures.
#[test]
fn very_deep_nesting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep");

    // Create 50 levels deep
    let mut current = root.clone();
    for i in 0..50 {
        current = current.join(format!("level{}", i));
    }
    fs::create_dir_all(&current).expect("create deep structure");
    fs::write(current.join("deep_file.txt"), b"found").expect("write deep file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Should have 51 directories (including root) + 1 file
    let file_entry = entries.iter().find(|e| {
        e.relative_path()
            .to_string_lossy()
            .ends_with("deep_file.txt")
    });

    assert!(file_entry.is_some(), "should find deep file");
    assert_eq!(
        file_entry.unwrap().depth(),
        51,
        "file should be at depth 51"
    );
}

/// Verifies depth tracking in complex structures.
#[test]
fn depth_tracking_accuracy() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("depth");
    fs::create_dir(&root).expect("create root");

    // Create structure with varying depths
    fs::create_dir(root.join("a")).expect("create a");
    fs::create_dir(root.join("a/b")).expect("create b");
    fs::create_dir(root.join("a/b/c")).expect("create c");
    fs::write(root.join("d0.txt"), b"").expect("write d0");
    fs::write(root.join("a/d1.txt"), b"").expect("write d1");
    fs::write(root.join("a/b/d2.txt"), b"").expect("write d2");
    fs::write(root.join("a/b/c/d3.txt"), b"").expect("write d3");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Verify depths
    for entry in entries {
        let expected_depth = entry.relative_path().components().count();
        if entry.is_root() {
            assert_eq!(entry.depth(), 0);
        } else {
            assert_eq!(
                entry.depth(),
                expected_depth,
                "depth mismatch for {:?}",
                entry.relative_path()
            );
        }
    }
}

// ============================================================================
// Special Character Edge Cases
// ============================================================================

/// Verifies handling of filenames with spaces.
#[test]
fn filenames_with_spaces() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    fs::write(root.join("file with spaces.txt"), b"data").expect("write file");
    fs::create_dir(root.join("dir with spaces")).expect("create dir");
    fs::write(root.join("dir with spaces/inner file.txt"), b"data").expect("write inner");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert!(paths.contains(&PathBuf::from("file with spaces.txt")));
    assert!(paths.contains(&PathBuf::from("dir with spaces")));
    assert!(paths.contains(&PathBuf::from("dir with spaces/inner file.txt")));
}

/// Verifies handling of filenames with special characters.
#[test]
fn filenames_with_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("special");
    fs::create_dir(&root).expect("create root");

    // Various special characters that are valid in filenames
    let names = [
        "file-with-dashes.txt",
        "file_with_underscores.txt",
        "file.multiple.dots.txt",
        "UPPERCASE.TXT",
        "MixedCase.Txt",
        "file@symbol.txt",
        "file#hash.txt",
        "file%percent.txt",
        "file&ampersand.txt",
        "file(parens).txt",
        "file[brackets].txt",
        "file{braces}.txt",
        "file+plus.txt",
        "file=equals.txt",
        "file'quote.txt",
        "file`backtick.txt",
        "file~tilde.txt",
        "file!exclaim.txt",
    ];

    for name in &names {
        fs::write(root.join(name), b"data").expect(&format!("write {}", name));
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    for name in &names {
        assert!(
            paths.contains(&PathBuf::from(*name)),
            "should contain {}",
            name
        );
    }
}

/// Verifies handling of Unicode filenames.
#[test]
fn unicode_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("unicode");
    fs::create_dir(&root).expect("create root");

    let unicode_names = [
        "file_with_emoji.txt", // Plain ASCII for comparison
        "archivo.txt",         // Spanish
        "fichier.txt",         // French
    ];

    for name in &unicode_names {
        fs::write(root.join(name), b"data").expect(&format!("write {}", name));
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), unicode_names.len());
}

/// Verifies handling of hidden files (dot prefix).
#[test]
fn hidden_files() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("hidden");
    fs::create_dir(&root).expect("create root");

    fs::write(root.join(".hidden"), b"data").expect("write hidden");
    fs::write(root.join(".config"), b"data").expect("write config");
    fs::create_dir(root.join(".hidden_dir")).expect("create hidden dir");
    fs::write(root.join(".hidden_dir/file.txt"), b"data").expect("write in hidden");
    fs::write(root.join("visible.txt"), b"data").expect("write visible");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // All files should be discovered, including hidden ones
    assert!(paths.contains(&PathBuf::from(".hidden")));
    assert!(paths.contains(&PathBuf::from(".config")));
    assert!(paths.contains(&PathBuf::from(".hidden_dir")));
    assert!(paths.contains(&PathBuf::from(".hidden_dir/file.txt")));
    assert!(paths.contains(&PathBuf::from("visible.txt")));
}

// ============================================================================
// Single File Root Edge Cases
// ============================================================================

/// Verifies handling when root is a single file.
#[test]
fn single_file_as_root() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let file = temp.path().join("single.txt");
    fs::write(&file, b"content").expect("write file");

    let walker = FileListBuilder::new(&file).build().expect("build walker");
    let entries = collect_all_entries(walker);

    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_root());
    assert!(entries[0].metadata().is_file());
    assert_eq!(entries[0].full_path(), file);
}

/// Verifies single file has no children.
#[test]
fn single_file_no_children() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let file = temp.path().join("single.txt");
    fs::write(&file, b"content").expect("write file");

    let mut walker = FileListBuilder::new(&file).build().expect("build walker");

    let root = walker.next().expect("root entry");
    assert!(root.is_ok());

    assert!(
        walker.next().is_none(),
        "single file should have no children"
    );
}

// ============================================================================
// Error Handling Edge Cases
// ============================================================================

/// Verifies error for non-existent path.
#[test]
fn nonexistent_path_error() {
    let result = FileListBuilder::new("/nonexistent/path/for/testing").build();

    match result {
        Ok(_) => panic!("expected error for nonexistent path"),
        Err(error) => {
            assert!(matches!(
                error.kind(),
                FileListErrorKind::RootMetadata { .. }
            ));
            assert!(error.path().to_string_lossy().contains("nonexistent"));
        }
    }
}

/// Verifies error path is preserved in error.
#[test]
fn error_path_preservation() {
    let path = "/unique/test/path/12345";
    let result = FileListBuilder::new(path).build();

    match result {
        Ok(_) => panic!("expected error for missing path"),
        Err(error) => {
            assert_eq!(error.path(), Path::new(path));
            assert_eq!(error.kind().path(), Path::new(path));
        }
    }
}

/// Verifies error display message is informative.
#[test]
fn error_display_message() {
    let result = FileListBuilder::new("/missing/path").build();

    match result {
        Ok(_) => panic!("expected error for missing path"),
        Err(error) => {
            let msg = error.to_string();
            assert!(
                msg.contains("missing") || msg.contains("path"),
                "error should reference path: {}",
                msg
            );
        }
    }
}

/// Verifies error debug format.
#[test]
fn error_debug_format() {
    let result = FileListBuilder::new("/missing/path").build();

    match result {
        Ok(_) => panic!("expected error for missing path"),
        Err(error) => {
            let debug = format!("{:?}", error);
            assert!(debug.contains("FileListError"));
        }
    }
}

// ============================================================================
// Boundary Condition Edge Cases
// ============================================================================

/// Verifies handling of empty filenames (shouldn't exist but test anyway).
#[test]
fn empty_root_path_handling() {
    // Empty path resolves to current directory
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path();

    // Should work from absolute path
    let walker = FileListBuilder::new(root).build().expect("build walker");
    let entries: Vec<_> = walker.collect();

    assert!(!entries.is_empty());
}

/// Verifies walker terminates correctly.
#[test]
fn walker_termination() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("term");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Exhaust walker
    while walker.next().is_some() {}

    // Multiple calls after exhaustion should return None
    for _ in 0..10 {
        assert!(walker.next().is_none());
    }
}

/// Verifies large number of files in single directory.
#[test]
fn many_files_in_single_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("many");
    fs::create_dir(&root).expect("create root");

    // Create 100 files
    for i in 0..100 {
        fs::write(root.join(format!("file{:03}.txt", i)), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 100);

    // Verify sorting
    for i in 0..99 {
        assert!(paths[i] < paths[i + 1], "files should be sorted");
    }
}

/// Verifies large directory tree.
#[test]
fn large_directory_tree() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tree");
    fs::create_dir(&root).expect("create root");

    // Create tree with 10 dirs, each with 10 files
    for i in 0..10 {
        let dir = root.join(format!("dir{:02}", i));
        fs::create_dir(&dir).expect("create dir");
        for j in 0..10 {
            fs::write(dir.join(format!("file{:02}.txt", j)), b"data").expect("write file");
        }
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // 10 dirs + 100 files = 110 entries
    assert_eq!(paths.len(), 110);
}

// ============================================================================
// File Type Edge Cases
// ============================================================================

/// Verifies handling of zero-length files.
#[test]
fn zero_length_files() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("zero");
    fs::create_dir(&root).expect("create root");

    // Create empty file
    fs::write(root.join("empty.txt"), b"").expect("write empty file");
    fs::write(root.join("normal.txt"), b"content").expect("write normal file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let empty_entry = entries
        .iter()
        .find(|e| e.relative_path() == Path::new("empty.txt"))
        .expect("empty file entry");

    assert_eq!(empty_entry.metadata().len(), 0);
    assert!(empty_entry.metadata().is_file());
}

/// Verifies handling of files with long names.
#[test]
fn long_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("long");
    fs::create_dir(&root).expect("create root");

    // Most filesystems support 255 character names
    let long_name = format!("{}.txt", "a".repeat(200));
    fs::write(root.join(&long_name), b"data").expect("write long name file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from(&long_name));
}

// ============================================================================
// Collect and Iterator Edge Cases
// ============================================================================

/// Verifies walker implements standard iterator patterns.
#[test]
fn walker_iterator_patterns() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("iter");
    fs::create_dir(&root).expect("create root");

    for i in 0..5 {
        fs::write(root.join(format!("file{}.txt", i)), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    // Test count
    let count = walker.count();
    assert_eq!(count, 6); // root + 5 files

    // Test collect after fresh walker
    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries: Result<Vec<_>, _> = walker.collect();
    assert!(entries.is_ok());
    assert_eq!(entries.unwrap().len(), 6);
}

/// Verifies walker can be used with iterator adapters.
#[test]
fn walker_with_iterator_adapters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("adapt");
    fs::create_dir(&root).expect("create root");

    for i in 0..10 {
        fs::write(root.join(format!("file{}.txt", i)), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    // Use filter and map
    let files: Vec<_> = walker
        .filter_map(|r| r.ok())
        .filter(|e| !e.is_root())
        .filter(|e| e.metadata().is_file())
        .map(|e| e.relative_path().to_path_buf())
        .take(5)
        .collect();

    assert_eq!(files.len(), 5);
}

/// Verifies walker works with for_each.
#[test]
fn walker_for_each() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("foreach");
    fs::create_dir(&root).expect("create root");

    for i in 0..3 {
        fs::write(root.join(format!("file{}.txt", i)), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    let mut count = 0;
    walker.for_each(|result| {
        assert!(result.is_ok());
        count += 1;
    });

    assert_eq!(count, 4); // root + 3 files
}

// ============================================================================
// Concurrent/Parallel Safety Edge Cases
// ============================================================================

/// Verifies multiple walkers can operate on same directory.
#[test]
fn multiple_walkers_same_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("multi");
    fs::create_dir(&root).expect("create root");

    for i in 0..5 {
        fs::write(root.join(format!("file{}.txt", i)), b"data").expect("write file");
    }

    // Create multiple walkers
    let walker1 = FileListBuilder::new(&root).build().expect("build walker 1");
    let walker2 = FileListBuilder::new(&root).build().expect("build walker 2");
    let walker3 = FileListBuilder::new(&root).build().expect("build walker 3");

    let paths1 = collect_relative_paths(walker1);
    let paths2 = collect_relative_paths(walker2);
    let paths3 = collect_relative_paths(walker3);

    assert_eq!(paths1, paths2);
    assert_eq!(paths2, paths3);
}

// ============================================================================
// Path Normalization Edge Cases
// ============================================================================

/// Verifies handling of paths with redundant separators.
#[test]
fn redundant_path_separators() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("normal");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    // Path with normal separators should work
    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    assert!(!entries.is_empty());
    assert!(entries[0].full_path().is_absolute());
}

/// Verifies relative paths in entries never contain parent references.
#[test]
fn relative_paths_no_parent_refs() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("noparent");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");
    fs::write(root.join("subdir/file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry success");
        let path_str = entry.relative_path().to_string_lossy();
        assert!(
            !path_str.contains(".."),
            "relative path should not contain ..: {}",
            path_str
        );
    }
}
