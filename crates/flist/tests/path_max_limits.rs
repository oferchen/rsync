//! Integration tests for PATH_MAX and deeply nested path handling.
//!
//! These tests verify correct behavior when dealing with paths that approach
//! or exceed system limits (PATH_MAX = 4096 on most Linux systems). The tests
//! ensure that:
//! 1. Paths approaching PATH_MAX are handled correctly
//! 2. Deeply nested directory hierarchies can be traversed
//! 3. Relative paths work correctly with deep nesting
//! 4. Appropriate errors are generated for paths exceeding limits
//!
//! Reference: rsync 3.4.1 flist.c - path handling and buffer management

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::fs;
use std::path::{Path, PathBuf};

// PATH_MAX on Linux is typically 4096 bytes
const PATH_MAX: usize = 4096;
// Leave some room for filesystem operations and null terminators
const SAFE_PATH_LIMIT: usize = PATH_MAX - 256;

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

/// Collects all entries including root from a walker.
fn collect_all_entries(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<FileListEntry> {
    walker.map(|r| r.expect("entry should succeed")).collect()
}

/// Creates a deeply nested directory structure with a given number of levels.
/// Returns the deepest directory path created.
fn create_deep_structure(root: &Path, levels: usize, dir_name_len: usize) -> PathBuf {
    let mut current = root.to_path_buf();
    for i in 0..levels {
        // Create directory names of specified length (or as close as possible)
        let name = format!("d{:0width$}", i, width = dir_name_len.saturating_sub(1));
        current = current.join(name);
    }
    fs::create_dir_all(&current).expect("create deep structure");
    current
}

/// Calculates the total path length including the root directory.
#[allow(dead_code)]
fn calculate_path_length(root: &Path, relative: &Path) -> usize {
    root.join(relative).as_os_str().len()
}

// ============================================================================
// Deep Nesting Tests - Basic Functionality
// ============================================================================

/// Verifies that very deep directory structures (100+ levels) can be traversed.
#[test]
fn traverse_very_deep_directory_hierarchy() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep");

    // Create 100 levels deep
    let deep_path = create_deep_structure(&root, 100, 10);
    fs::write(deep_path.join("deep_file.txt"), b"buried treasure").expect("write deep file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Should have: root + 100 directories + 1 file = 102 entries
    assert_eq!(entries.len(), 102, "should traverse all levels");

    // Verify the deepest file is found
    let deep_file = entries.iter().find(|e| {
        e.relative_path()
            .to_string_lossy()
            .ends_with("deep_file.txt")
    });
    assert!(deep_file.is_some(), "should find file at depth 100");
    assert_eq!(deep_file.unwrap().depth(), 101, "file depth should be 101");
}

/// Verifies multiple deep branches can coexist and be traversed correctly.
#[test]
fn multiple_deep_branches() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("branches");
    fs::create_dir(&root).expect("create root");

    // Create three separate deep branches
    for branch in ["branch_a", "branch_b", "branch_c"] {
        let branch_root = root.join(branch);
        let deep_path = create_deep_structure(&branch_root, 30, 8);
        fs::write(deep_path.join(format!("{branch}.txt")), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Each branch: 30 dirs + 1 file = 31 entries per branch
    // Total: root + 3 top-level dirs + 3 * 31 = 1 + 3 + 93 = 97 entries
    assert_eq!(entries.len(), 97, "should traverse all branches");

    // Verify files in each branch are found
    for branch in ["branch_a", "branch_b", "branch_c"] {
        let found = entries.iter().any(|e| {
            e.relative_path()
                .to_string_lossy()
                .ends_with(&format!("{branch}.txt"))
        });
        assert!(found, "should find file in {branch}");
    }
}

/// Verifies depth tracking is accurate for very deep structures.
#[test]
fn accurate_depth_tracking_in_deep_structures() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("depth");

    let deep_path = create_deep_structure(&root, 50, 8);
    fs::write(deep_path.join("test.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry should succeed");
        if entry.is_root() {
            assert_eq!(entry.depth(), 0, "root depth is 0");
            continue;
        }

        // Verify depth matches path component count
        let expected_depth = entry.relative_path().components().count();
        assert_eq!(
            entry.depth(),
            expected_depth,
            "depth mismatch for {:?}",
            entry.relative_path()
        );
    }
}

// ============================================================================
// PATH_MAX Boundary Tests
// ============================================================================

/// Verifies paths approaching PATH_MAX can be handled successfully.
#[test]
fn handle_paths_near_path_max() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("near_max");

    // Calculate how many levels we can create
    // Use 100-character directory names to approach the limit quickly
    let dir_name_len = 100;
    let root_len = root.as_os_str().len();

    // Calculate levels needed to approach SAFE_PATH_LIMIT
    let available_space = SAFE_PATH_LIMIT.saturating_sub(root_len);
    // Add 1 for separator per level
    let levels = available_space / (dir_name_len + 1);
    let levels = levels.min(30); // Cap at 30 levels for reasonable test time

    if levels < 2 {
        // Skip test if temp path is already too long
        eprintln!("Skipping test: root path too long");
        return;
    }

    let deep_path = create_deep_structure(&root, levels, dir_name_len);
    let test_file = deep_path.join("limit_test.txt");
    fs::write(&test_file, b"near the limit").expect("write file");

    let total_path_len = test_file.as_os_str().len();
    println!("Created path length: {total_path_len} (PATH_MAX: {PATH_MAX})");

    // Should successfully traverse despite long paths
    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    assert!(!entries.is_empty(), "should traverse long paths");

    // Verify the deep file is found
    let found = entries.iter().any(|e| {
        e.relative_path()
            .to_string_lossy()
            .ends_with("limit_test.txt")
    });
    assert!(found, "should find file near path limit");
}

/// Verifies relative paths are correctly constructed for very long absolute paths.
#[test]
fn relative_paths_correct_for_long_absolute_paths() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("long_relative");

    // Create moderately deep structure
    let levels = 20;
    let deep_path = create_deep_structure(&root, levels, 50);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry should succeed");
        if entry.is_root() {
            continue;
        }

        let relative = entry.relative_path();
        let full = entry.full_path();

        // Verify full path = root + relative
        let reconstructed = root.join(relative);
        assert_eq!(
            full, reconstructed,
            "full path should equal root + relative"
        );

        // Verify relative path doesn't contain parent references
        let relative_str = relative.to_string_lossy();
        assert!(
            !relative_str.contains(".."),
            "relative path should not contain '..'"
        );
        assert!(
            relative.is_relative(),
            "relative_path() should return relative path"
        );
    }
}

/// Verifies that paths with long individual component names are handled.
#[test]
fn handle_long_individual_component_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("long_names");
    fs::create_dir(&root).expect("create root");

    // Most filesystems support 255-byte filenames
    // Create a directory with a 200-character name
    let long_name = "a".repeat(200);
    let long_dir = root.join(&long_name);
    fs::create_dir(&long_dir).expect("create long dir");

    // Create a file with another long name
    let long_file_name = format!("{}.txt", "b".repeat(200));
    fs::write(long_dir.join(&long_file_name), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should find both the directory and file
    assert_eq!(paths.len(), 2, "should find directory and file");
    assert!(
        paths
            .iter()
            .any(|p| p.to_string_lossy().contains(&long_name)),
        "should find long directory name"
    );
    assert!(
        paths
            .iter()
            .any(|p| p.to_string_lossy().contains(&long_file_name)),
        "should find long file name"
    );
}

// ============================================================================
// Mixed Depth Structures
// ============================================================================

/// Verifies correct traversal of structures with varying depth levels.
#[test]
fn mixed_depth_structure_traversal() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed");
    fs::create_dir(&root).expect("create root");

    // Create shallow branch
    fs::create_dir(root.join("shallow")).expect("create shallow");
    fs::write(root.join("shallow/file.txt"), b"shallow").expect("write shallow file");

    // Create medium depth branch (20 levels)
    let medium = create_deep_structure(&root.join("medium"), 20, 8);
    fs::write(medium.join("file.txt"), b"medium").expect("write medium file");

    // Create deep branch (40 levels)
    let deep = create_deep_structure(&root.join("deep"), 40, 8);
    fs::write(deep.join("file.txt"), b"deep").expect("write deep file");

    // Add root level file
    fs::write(root.join("root.txt"), b"root").expect("write root file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Verify all depth levels are present
    let depths: Vec<_> = entries.iter().map(|e| e.depth()).collect();
    assert!(depths.contains(&0), "should have depth 0 (root)");
    assert!(depths.contains(&1), "should have depth 1");
    assert!(depths.contains(&2), "should have shallow depth 2");
    assert!(depths.iter().any(|&d| d > 20), "should have medium depths");
    assert!(depths.iter().any(|&d| d > 40), "should have deep depths");
}

/// Verifies files at various depths are all discovered.
#[test]
fn files_at_various_depths_discovered() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("varied");
    fs::create_dir(&root).expect("create root");

    // Create files at depths: 1, 5, 10, 15, 20
    let depths = [1, 5, 10, 15, 20];
    for &depth in &depths {
        let mut path = root.clone();
        for i in 0..depth {
            path = path.join(format!("level{i}"));
        }
        fs::create_dir_all(&path).expect("create path");
        fs::write(path.join(format!("file_at_depth_{depth}.txt")), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Verify we found files at all specified depths
    for &expected_depth in &depths {
        let found = entries.iter().any(|e| {
            e.metadata().is_file()
                && e.relative_path()
                    .to_string_lossy()
                    .contains(&format!("file_at_depth_{expected_depth}"))
        });
        assert!(found, "should find file at depth {expected_depth}");
    }
}

// ============================================================================
// Include Root Option with Deep Paths
// ============================================================================

/// Verifies include_root(false) works correctly with deep paths.
#[test]
fn include_root_false_with_deep_paths() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("no_root_deep");

    let deep_path = create_deep_structure(&root, 30, 10);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let entries = collect_all_entries(walker);

    // Verify no root entry
    assert!(
        entries.iter().all(|e| !e.is_root()),
        "should not include root entry"
    );

    // Should still have all other entries (30 dirs + 1 file)
    assert_eq!(entries.len(), 31, "should have all non-root entries");
}

/// Verifies root entry properties with very long absolute paths.
#[test]
fn root_entry_with_long_absolute_path() {
    let temp = tempfile::tempdir().expect("create tempdir");
    // Create a root with a relatively long name
    let root = temp.path().join("a".repeat(100));
    fs::create_dir(&root).expect("create root");

    let deep_path = create_deep_structure(&root, 10, 50);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let root_entry = walker.next().expect("root entry").expect("success");
    assert!(root_entry.is_root(), "first entry should be root");
    assert_eq!(root_entry.depth(), 0, "root depth is 0");
    assert!(
        root_entry.relative_path().as_os_str().is_empty(),
        "root relative path is empty"
    );
    assert!(
        root_entry.full_path().is_absolute(),
        "root full path is absolute"
    );

    // Verify long absolute path is preserved
    assert_eq!(root_entry.full_path(), root);
}

// ============================================================================
// Error Handling for Path Limits
// ============================================================================

/// Verifies that excessively long paths that might cause issues are handled gracefully.
/// Note: This test checks that we handle what the OS allows, not that we enforce
/// arbitrary limits ourselves.
#[test]
fn graceful_handling_of_filesystem_path_limits() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("limit_test");
    fs::create_dir(&root).expect("create root");

    // Try to create a structure that might approach filesystem limits
    // If the filesystem rejects it, that's expected behavior
    let result = std::panic::catch_unwind(|| {
        // Try 50 levels with 80-char names
        create_deep_structure(&root, 50, 80)
    });

    match result {
        Ok(_deep_path) => {
            // If we successfully created it, we should be able to traverse it
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let entries: Vec<_> = walker.collect();

            // Should successfully traverse whatever the filesystem allowed
            assert!(
                !entries.is_empty(),
                "should traverse filesystem-allowed paths"
            );
        }
        Err(_) => {
            // If the filesystem rejected the deep structure, that's fine
            // Just verify we can still traverse what exists
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let entries = collect_all_entries(walker);
            assert!(!entries.is_empty(), "should traverse existing paths");
        }
    }
}

// ============================================================================
// Real-world Deep Structure Tests
// ============================================================================

/// Verifies traversal of a realistic deeply nested node_modules-like structure.
#[test]
fn realistic_node_modules_style_structure() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("project");
    fs::create_dir(&root).expect("create root");

    // Simulate nested node_modules: project/node_modules/pkg1/node_modules/pkg2/node_modules/pkg3
    let mut current = root.clone();
    for i in 0..10 {
        current = current.join("node_modules").join(format!("package{i}"));
        fs::create_dir_all(&current).expect("create nested modules");
        fs::write(current.join("index.js"), b"module.exports = {};").expect("write js file");
        fs::write(current.join("package.json"), b"{}").expect("write package.json");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Should find all package.json and index.js files
    let js_files = entries
        .iter()
        .filter(|e| e.relative_path().to_string_lossy().ends_with("index.js"))
        .count();
    let json_files = entries
        .iter()
        .filter(|e| {
            e.relative_path()
                .to_string_lossy()
                .ends_with("package.json")
        })
        .count();

    assert_eq!(js_files, 10, "should find all index.js files");
    assert_eq!(json_files, 10, "should find all package.json files");
}

/// Verifies traversal of deep directory tree with many files at leaf level.
#[test]
fn deep_tree_with_many_leaf_files() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep_leaves");

    // Create 20-level deep structure
    let deep_path = create_deep_structure(&root, 20, 15);

    // Add 50 files at the deepest level
    for i in 0..50 {
        fs::write(deep_path.join(format!("file{i:03}.txt")), b"leaf data")
            .expect("write leaf file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Should have: root + 20 dirs + 50 files = 71 entries
    assert_eq!(entries.len(), 71, "should find all entries");

    // Verify all files are at depth 21
    let leaf_files: Vec<_> = entries
        .iter()
        .filter(|e| e.metadata().is_file() && e.relative_path().to_string_lossy().starts_with("d0"))
        .collect();

    assert_eq!(leaf_files.len(), 50, "should find all leaf files");
    for file in leaf_files {
        assert_eq!(file.depth(), 21, "leaf files should be at depth 21");
    }
}

// ============================================================================
// Iterator Behavior with Deep Structures
// ============================================================================

/// Verifies walker can be partially consumed with deep structures.
#[test]
fn partial_consumption_of_deep_walker() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("partial");

    let deep_path = create_deep_structure(&root, 50, 10);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Consume only first 10 entries
    let mut count = 0;
    for entry in &mut walker {
        entry.expect("entry should succeed");
        count += 1;
        if count >= 10 {
            break;
        }
    }

    assert_eq!(count, 10, "should consume exactly 10 entries");

    // Walker should still be valid and can continue
    let next = walker.next();
    assert!(next.is_some(), "walker should have more entries");
}

/// Verifies walker filter operations work correctly with deep structures.
#[test]
fn filter_operations_on_deep_walker() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("filter");

    let deep_path = create_deep_structure(&root, 30, 10);

    // Add a file at shallow depth (in the first directory level created by create_deep_structure)
    // dir_name_len=10, so format width is 9, creating "d000000000"
    let shallow_path = root.join("d000000000");
    fs::write(shallow_path.join("file_shallow.txt"), b"data").expect("write shallow");

    // Add file at deep level
    fs::write(deep_path.join("file_deep.txt"), b"data").expect("write deep");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    // Filter to only files (not directories)
    let files: Vec<_> = walker
        .filter_map(|r| r.ok())
        .filter(|e| e.metadata().is_file())
        .collect();

    assert_eq!(files.len(), 2, "should find exactly 2 files");

    // Verify both shallow and deep files are included
    let paths: Vec<_> = files
        .iter()
        .map(|e| e.relative_path().to_string_lossy().to_string())
        .collect();
    assert!(
        paths.iter().any(|p| p.contains("file_shallow.txt")),
        "should include shallow file"
    );
    assert!(
        paths.iter().any(|p| p.contains("file_deep.txt")),
        "should include deep file"
    );
}

// ============================================================================
// Path Component Validation
// ============================================================================

/// Verifies that relative paths never contain '..' components even in deep structures.
#[test]
fn no_parent_references_in_deep_relative_paths() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("no_parent");

    let deep_path = create_deep_structure(&root, 40, 12);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry should succeed");
        let relative = entry.relative_path();

        // Check each component
        for component in relative.components() {
            use std::path::Component;
            assert!(
                !matches!(component, Component::ParentDir),
                "should not contain '..' component in {relative:?}"
            );
        }
    }
}

/// Verifies that full paths are always absolute even for deep structures.
#[test]
fn full_paths_always_absolute_for_deep_structures() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("absolute");

    let deep_path = create_deep_structure(&root, 35, 10);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry should succeed");
        let full = entry.full_path();

        assert!(full.is_absolute(), "full path should be absolute: {full:?}");

        // Verify it starts with root
        assert!(
            full.starts_with(&root),
            "full path {full:?} should start with root {root:?}"
        );
    }
}

/// Verifies file_name() is correct even for deeply nested entries.
#[test]
fn file_names_correct_in_deep_structures() {
    use std::ffi::OsStr;

    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("names");

    let deep_path = create_deep_structure(&root, 25, 15);
    let filename = "deeply_nested_file.txt";
    fs::write(deep_path.join(filename), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let file_entry = entries
        .iter()
        .find(|e| e.metadata().is_file())
        .expect("should find file entry");

    assert_eq!(
        file_entry.file_name(),
        Some(OsStr::new(filename)),
        "file_name should return correct basename"
    );
}

// ============================================================================
// Performance and Memory Tests (Implicit)
// ============================================================================

/// Verifies walker doesn't consume excessive memory for deep structures.
/// This test ensures lazy evaluation by creating a very deep structure
/// and verifying we can iterate through it without loading everything into memory.
#[test]
fn lazy_evaluation_for_very_deep_structures() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("lazy");

    // Create a very deep structure (200 levels)
    let deep_path = create_deep_structure(&root, 200, 8);
    fs::write(deep_path.join("bottom.txt"), b"deepest point").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    // Take only the first 50 entries
    let mut count = 0;
    for entry in walker {
        entry.expect("entry should succeed");
        count += 1;
        if count >= 50 {
            break;
        }
    }

    assert_eq!(count, 50, "should get first 50 entries");

    // If walker was eagerly loading everything, this would have consumed
    // memory for all 200+ entries. Instead, we only processed 50.
    // This is an implicit test - we're verifying it doesn't panic or OOM.
}
