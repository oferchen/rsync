//! Integration tests for file list building from directory trees.
//!
//! These tests verify that [`FileListBuilder`] and [`FileListWalker`] correctly
//! enumerate filesystem entries, matching upstream rsync's `flist.c` behavior
//! for directory scanning (see `send_file_list()` at line 2192 and
//! `send_file_name()` at line 1080).
//!
//! Reference: rsync 3.4.1 flist.c

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// ============================================================================
// Helper Functions
// ============================================================================

/// Collects relative paths from a walker, skipping the root entry.
fn collect_relative_paths(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in walker {
        let entry = entry.expect("walker entry should succeed");
        if entry.is_root() {
            continue;
        }
        paths.push(entry.relative_path().to_path_buf());
    }
    paths
}

/// Collects all entries including root from a walker.
fn collect_all_entries(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<FileListEntry> {
    walker.map(|r| r.expect("entry should succeed")).collect()
}

// ============================================================================
// Basic Directory Tree Construction Tests
// ============================================================================

/// Verifies that an empty directory yields only the root entry.
///
/// Upstream rsync emits the root directory entry when scanning an empty
/// directory. This test ensures our implementation matches that behavior.
#[test]
fn empty_directory_yields_root_only() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("empty");
    fs::create_dir(&root).expect("create empty dir");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    assert_eq!(
        entries.len(),
        1,
        "empty directory should yield exactly one entry"
    );
    assert!(entries[0].is_root(), "the single entry should be the root");
    assert!(entries[0].metadata().is_dir(), "root should be a directory");
}

/// Verifies that a single file in a directory is correctly discovered.
#[test]
fn single_file_in_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("single");
    fs::create_dir(&root).expect("create root dir");
    fs::write(root.join("file.txt"), b"content").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths, vec![PathBuf::from("file.txt")]);
}

/// Verifies that multiple files in a directory are discovered in sorted order.
///
/// Upstream rsync sorts directory entries lexicographically (flist.c line 200:
/// `entries.sort()`). This ensures deterministic output across platforms.
#[test]
fn multiple_files_sorted_alphabetically() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("multi");
    fs::create_dir(&root).expect("create root dir");

    // Create files in non-alphabetical order
    fs::write(root.join("zebra.txt"), b"z").expect("write zebra");
    fs::write(root.join("apple.txt"), b"a").expect("write apple");
    fs::write(root.join("mango.txt"), b"m").expect("write mango");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![
            PathBuf::from("apple.txt"),
            PathBuf::from("mango.txt"),
            PathBuf::from("zebra.txt"),
        ],
        "files should be sorted alphabetically"
    );
}

/// Verifies nested directory structures are traversed depth-first.
///
/// Upstream rsync uses depth-first traversal for directory scanning. When
/// entering a directory, all its contents are processed before moving to
/// the next sibling.
#[test]
fn nested_directories_depth_first_traversal() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("nested");
    fs::create_dir(&root).expect("create root");

    // Create structure:
    // root/
    //   a/
    //     inner.txt
    //   b/
    //   c.txt
    let dir_a = root.join("a");
    let dir_b = root.join("b");
    fs::create_dir(&dir_a).expect("create dir a");
    fs::create_dir(&dir_b).expect("create dir b");
    fs::write(dir_a.join("inner.txt"), b"data").expect("write inner");
    fs::write(root.join("c.txt"), b"data").expect("write c");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Depth-first: a is processed, then its contents, then b, then c.txt
    assert_eq!(
        paths,
        vec![
            PathBuf::from("a"),
            PathBuf::from("a/inner.txt"),
            PathBuf::from("b"),
            PathBuf::from("c.txt"),
        ]
    );
}

/// Verifies deeply nested directory structures are traversed correctly.
#[test]
fn deeply_nested_directories() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep");

    // Create: root/a/b/c/d/e/file.txt
    let deep_path = root.join("a/b/c/d/e");
    fs::create_dir_all(&deep_path).expect("create deep structure");
    fs::write(deep_path.join("file.txt"), b"deep content").expect("write file");

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
            PathBuf::from("a/b/c/d/e/file.txt"),
        ]
    );
}

/// Verifies mixed files and directories at multiple levels.
#[test]
fn mixed_files_and_directories_at_multiple_levels() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed");
    fs::create_dir(&root).expect("create root");

    // Create structure:
    // root/
    //   dir1/
    //     file1a.txt
    //     subdir/
    //       deep.txt
    //   dir2/
    //     file2a.txt
    //   root_file.txt
    let dir1 = root.join("dir1");
    let subdir = dir1.join("subdir");
    let dir2 = root.join("dir2");

    fs::create_dir(&dir1).expect("create dir1");
    fs::create_dir(&subdir).expect("create subdir");
    fs::create_dir(&dir2).expect("create dir2");

    fs::write(dir1.join("file1a.txt"), b"1a").expect("write file1a");
    fs::write(subdir.join("deep.txt"), b"deep").expect("write deep");
    fs::write(dir2.join("file2a.txt"), b"2a").expect("write file2a");
    fs::write(root.join("root_file.txt"), b"root").expect("write root_file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![
            PathBuf::from("dir1"),
            PathBuf::from("dir1/file1a.txt"),
            PathBuf::from("dir1/subdir"),
            PathBuf::from("dir1/subdir/deep.txt"),
            PathBuf::from("dir2"),
            PathBuf::from("dir2/file2a.txt"),
            PathBuf::from("root_file.txt"),
        ]
    );
}

// ============================================================================
// Root Entry Behavior Tests
// ============================================================================

/// Verifies root entry properties when starting from a directory.
#[test]
fn root_entry_properties_for_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root_test");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("child.txt"), b"data").expect("write child");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let root_entry = walker.next().expect("root entry").expect("success");
    assert!(root_entry.is_root(), "first entry should be root");
    assert_eq!(root_entry.depth(), 0, "root depth should be 0");
    assert!(root_entry.file_name().is_none(), "root has no file name");
    assert!(
        root_entry.relative_path().as_os_str().is_empty(),
        "root relative path should be empty"
    );
    assert!(
        root_entry.full_path().is_absolute(),
        "full path should be absolute"
    );
    assert!(root_entry.metadata().is_dir(), "root should be directory");
}

/// Verifies root entry when starting from a single file.
#[test]
fn root_entry_when_starting_from_file() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let file_path = temp.path().join("single_file.txt");
    fs::write(&file_path, b"content").expect("write file");

    let mut walker = FileListBuilder::new(&file_path)
        .build()
        .expect("build walker");

    let root_entry = walker.next().expect("root entry").expect("success");
    assert!(root_entry.is_root(), "entry should be root");
    assert!(root_entry.metadata().is_file(), "root should be file");
    assert_eq!(root_entry.full_path(), file_path);

    assert!(walker.next().is_none(), "no more entries after single file");
}

/// Verifies include_root(false) skips the root entry.
#[test]
fn include_root_false_skips_root_entry() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("no_root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let mut walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let first = walker.next().expect("first entry").expect("success");
    assert!(!first.is_root(), "first entry should not be root");
    assert_eq!(
        first.relative_path(),
        Path::new("file.txt"),
        "first entry should be the file"
    );
}

/// Verifies include_root(false) on empty directory yields no entries.
#[test]
fn include_root_false_empty_directory_yields_nothing() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("empty_no_root");
    fs::create_dir(&root).expect("create root");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let entries: Vec<_> = walker.collect();
    assert!(
        entries.is_empty(),
        "empty directory with no root should yield nothing"
    );
}

// ============================================================================
// Entry Metadata Tests
// ============================================================================

/// Verifies that entry metadata correctly reflects file properties.
#[test]
fn entry_metadata_reflects_file_properties() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("meta");
    fs::create_dir(&root).expect("create root");

    let file_content = b"test content with known size";
    fs::write(root.join("file.txt"), file_content).expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let file_entry = entries.iter().find(|e| !e.is_root()).expect("file entry");
    assert!(file_entry.metadata().is_file(), "entry should be file");
    assert_eq!(
        file_entry.metadata().len(),
        file_content.len() as u64,
        "file size should match content length"
    );
}

/// Verifies that directory entries have correct metadata.
#[test]
fn directory_entry_metadata() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dir_meta");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let subdir_entry = entries
        .iter()
        .find(|e| e.relative_path() == Path::new("subdir"))
        .expect("subdir entry");

    assert!(
        subdir_entry.metadata().is_dir(),
        "subdir should be directory"
    );
}

// ============================================================================
// Relative Path Construction Tests
// ============================================================================

/// Verifies relative paths are correctly constructed at various depths.
#[test]
fn relative_paths_at_various_depths() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("depth_test");
    fs::create_dir(&root).expect("create root");

    // Create structure with files at different depths
    fs::write(root.join("level1.txt"), b"1").expect("write level1");
    fs::create_dir(root.join("dir")).expect("create dir");
    fs::write(root.join("dir/level2.txt"), b"2").expect("write level2");
    fs::create_dir(root.join("dir/subdir")).expect("create subdir");
    fs::write(root.join("dir/subdir/level3.txt"), b"3").expect("write level3");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let mut paths_with_depths: Vec<_> = walker
        .map(|r| r.expect("success"))
        .filter(|e| !e.is_root())
        .map(|e| (e.relative_path().to_path_buf(), e.depth()))
        .collect();

    // Verify depth matches path component count
    for (path, depth) in &paths_with_depths {
        let expected_depth = path.components().count();
        assert_eq!(
            *depth, expected_depth,
            "depth {depth} should match component count {expected_depth} for {path:?}"
        );
    }

    // Verify specific paths
    paths_with_depths.sort();
    let paths: Vec<_> = paths_with_depths.iter().map(|(p, _)| p.clone()).collect();
    assert!(paths.contains(&PathBuf::from("level1.txt")));
    assert!(paths.contains(&PathBuf::from("dir/level2.txt")));
    assert!(paths.contains(&PathBuf::from("dir/subdir/level3.txt")));
}

/// Verifies file_name() returns the correct basename for each entry.
#[test]
fn file_name_returns_basename() {
    use std::ffi::OsStr;

    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("basename");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");
    fs::write(root.join("subdir/file.txt"), b"content").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let root_entry = entries.iter().find(|e| e.is_root()).expect("root");
    assert!(root_entry.file_name().is_none(), "root has no file_name");

    let subdir_entry = entries
        .iter()
        .find(|e| e.relative_path() == Path::new("subdir"))
        .expect("subdir");
    assert_eq!(subdir_entry.file_name(), Some(OsStr::new("subdir")));

    let file_entry = entries
        .iter()
        .find(|e| e.relative_path() == Path::new("subdir/file.txt"))
        .expect("file");
    assert_eq!(file_entry.file_name(), Some(OsStr::new("file.txt")));
}

// ============================================================================
// Full Path Verification Tests
// ============================================================================

/// Verifies full_path() returns absolute paths.
#[test]
fn full_path_is_always_absolute() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("abs_test");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");
    fs::write(root.join("subdir/file.txt"), b"content").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("success");
        assert!(
            entry.full_path().is_absolute(),
            "full_path {:?} should be absolute",
            entry.full_path()
        );
    }
}

/// Verifies full_path() points to the correct filesystem location.
#[test]
fn full_path_points_to_correct_location() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("loc_test");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"specific content").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let file_entry = entries
        .iter()
        .find(|e| e.relative_path() == Path::new("file.txt"))
        .expect("file entry");

    // Verify we can read the file using full_path
    let content = fs::read(file_entry.full_path()).expect("read file");
    assert_eq!(content, b"specific content");
}

// ============================================================================
// Exhaustion and Termination Tests
// ============================================================================

/// Verifies walker correctly terminates after processing all entries.
#[test]
fn walker_terminates_after_exhaustion() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("exhaust");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Exhaust the walker
    while walker.next().is_some() {}

    // Repeated calls should return None
    assert!(walker.next().is_none());
    assert!(walker.next().is_none());
    assert!(walker.next().is_none());
}

/// Verifies walker can be collected into a Vec.
#[test]
fn walker_can_be_collected() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("collect");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("a.txt"), b"a").expect("write a");
    fs::write(root.join("b.txt"), b"b").expect("write b");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries: Result<Vec<_>, _> = walker.collect();

    let entries = entries.expect("collection should succeed");
    assert_eq!(entries.len(), 3, "root + 2 files");
}

/// Verifies walker implements Iterator correctly with for-loop.
#[test]
fn walker_works_with_for_loop() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("forloop");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let mut count = 0;

    for entry in walker {
        entry.expect("entry should succeed");
        count += 1;
    }

    assert_eq!(count, 2, "should iterate over root + file");
}

// ============================================================================
// Builder Configuration Tests
// ============================================================================

/// Verifies builder can be cloned and produces equivalent walkers.
#[test]
fn builder_clone_produces_equivalent_walker() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("clone_test");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let builder = FileListBuilder::new(&root);
    let cloned = builder.clone();

    let paths1 = collect_relative_paths(builder.build().expect("build walker"));
    let paths2 = collect_relative_paths(cloned.build().expect("build cloned walker"));

    assert_eq!(paths1, paths2, "cloned builder should produce same paths");
}

/// Verifies builder debug format contains useful information.
#[test]
fn builder_debug_format() {
    let builder = FileListBuilder::new("/some/path")
        .follow_symlinks(true)
        .include_root(false);

    let debug = format!("{builder:?}");
    assert!(
        debug.contains("FileListBuilder"),
        "debug should contain type name"
    );
}

/// Verifies builder method chaining works correctly.
#[test]
fn builder_method_chaining() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("chain");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(false)
        .include_root(true)
        .follow_symlinks(true) // Override previous setting
        .include_root(false) // Override previous setting
        .build()
        .expect("build walker");

    // include_root(false) should be in effect
    let entries = collect_all_entries(walker);
    assert!(
        entries.iter().all(|e| !e.is_root()),
        "no root entry should be present"
    );
}

// ============================================================================
// Unique Entry Tests (No Duplicates)
// ============================================================================

/// Verifies that each directory entry is yielded exactly once.
#[test]
fn each_entry_yielded_exactly_once() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("unique");
    fs::create_dir(&root).expect("create root");

    // Create structure with multiple directories
    for name in ["a", "b", "c"] {
        let dir = root.join(name);
        fs::create_dir(&dir).expect("create dir");
        fs::write(dir.join("file.txt"), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Convert to set and check count matches
    let unique_paths: BTreeSet<_> = paths.iter().collect();
    assert_eq!(
        paths.len(),
        unique_paths.len(),
        "all paths should be unique, no duplicates"
    );
}
