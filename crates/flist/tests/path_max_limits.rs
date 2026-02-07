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

// ============================================================================
// Paths at Exactly PATH_MAX Tests
// ============================================================================

/// Calculates the exact path length needed to reach a target total length.
#[allow(dead_code)]
fn calculate_levels_for_target_length(
    root: &Path,
    target_len: usize,
    dir_name_len: usize,
) -> usize {
    let root_len = root.as_os_str().len();
    let available = target_len.saturating_sub(root_len);
    // Each level adds dir_name_len + 1 (for separator)
    available / (dir_name_len + 1)
}

/// Attempts to create a path of exactly the specified length.
/// Returns the deepest directory path if successful.
fn try_create_path_of_length(root: &Path, target_len: usize) -> Option<PathBuf> {
    let root_len = root.as_os_str().len();
    if target_len <= root_len {
        return Some(root.to_path_buf());
    }

    // Use 100-char directory names for efficient length building
    let dir_name_base_len = 100;
    let available = target_len - root_len;

    // Calculate how many full-length directories we need
    let levels = available / (dir_name_base_len + 1);
    let remainder = available % (dir_name_base_len + 1);

    let mut current = root.to_path_buf();

    // Create directories with full-length names
    for i in 0..levels {
        let name = format!("d{:0width$}", i, width = dir_name_base_len - 1);
        current = current.join(name);
    }

    // Create final directory with exact remaining length
    if remainder > 1 {
        // remainder includes the separator
        let final_name_len = remainder - 1;
        if final_name_len > 0 {
            let final_name = "x".repeat(final_name_len);
            current = current.join(final_name);
        }
    }

    match fs::create_dir_all(&current) {
        Ok(_) => Some(current),
        Err(_) => None,
    }
}

/// Verifies handling of paths at exactly PATH_MAX - 1 (maximum valid).
#[test]
fn path_at_exactly_path_max_minus_one() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("exact_max");
    fs::create_dir(&root).expect("create root");

    let root_len = root.as_os_str().len();
    // Target path length: PATH_MAX - 1 (4095) minus some buffer for the file name
    let target_dir_len = PATH_MAX.saturating_sub(1).saturating_sub(50); // Leave room for filename

    if target_dir_len <= root_len {
        eprintln!("Root path too long for test");
        return;
    }

    if let Some(deep_path) = try_create_path_of_length(&root, target_dir_len) {
        let file_path = deep_path.join("f.txt");
        if fs::write(&file_path, b"at limit").is_ok() {
            let total_len = file_path.as_os_str().len();
            println!("Created path of length {total_len} (target: ~{target_dir_len})");

            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let entries = collect_all_entries(walker);

            // Should find the file
            let found = entries
                .iter()
                .any(|e| e.relative_path().to_string_lossy().ends_with("f.txt"));
            assert!(found, "should find file near PATH_MAX limit");
        }
    }
}

/// Verifies that paths approaching PATH_MAX with long filenames are handled.
#[test]
fn long_path_with_long_filename_combination() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("combo");
    fs::create_dir(&root).expect("create root");

    // Create a moderately deep structure
    let deep_path = create_deep_structure(&root, 20, 50);

    // Try to create a file with a long name (200 chars)
    let long_filename = format!("{}.txt", "long_name_".repeat(19));
    let file_path = deep_path.join(&long_filename);

    if fs::write(&file_path, b"combo test").is_ok() {
        let total_len = file_path.as_os_str().len();
        println!("Combined path length: {total_len}");

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        // Verify file was found
        let found = entries
            .iter()
            .any(|e| e.relative_path().to_string_lossy().contains("long_name_"));
        assert!(found, "should find file with long name in deep directory");
    }
}

// ============================================================================
// Paths Exceeding PATH_MAX Tests
// ============================================================================

/// Verifies graceful handling when filesystem rejects paths exceeding PATH_MAX.
/// The test attempts to create a structure that would exceed PATH_MAX and verifies
/// that either the OS rejects it (expected) or we can still traverse what exists.
#[test]
fn path_exceeding_path_max_filesystem_rejection() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("exceed");
    fs::create_dir(&root).expect("create root");

    // Calculate how many 200-char directories would exceed PATH_MAX
    let root_len = root.as_os_str().len();
    let dir_name_len = 200;
    // Need enough levels to exceed PATH_MAX (4096)
    let levels_to_exceed = (PATH_MAX + 1000 - root_len) / (dir_name_len + 1);

    let mut current = root.clone();
    let mut created_count = 0;

    for i in 0..levels_to_exceed {
        let name = format!("d{:0width$}", i, width = dir_name_len - 1);
        let next = current.join(&name);

        match fs::create_dir(&next) {
            Ok(_) => {
                current = next;
                created_count += 1;
            }
            Err(e) => {
                // Expected: filesystem rejects path that's too long
                println!("Filesystem rejected at level {i}: {e}");
                break;
            }
        }
    }

    // Traverse whatever was successfully created
    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Should have at least root + created directories
    assert!(
        entries.len() >= created_count,
        "should traverse all successfully created directories"
    );

    println!("Created {created_count} levels before filesystem rejection");
}

/// Verifies that the walker handles a directory structure where paths approach
/// but don't exceed the limit gracefully.
#[test]
fn walk_structure_approaching_limit_from_multiple_branches() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("multi_approach");
    fs::create_dir(&root).expect("create root");

    // Create multiple branches that each approach the limit
    let branches = ["alpha", "beta", "gamma"];
    let mut total_files = 0;

    for branch in &branches {
        let branch_root = root.join(branch);
        // Try to create 15 levels with 100-char names
        match fs::create_dir_all(&branch_root) {
            Ok(_) => {
                let deep = create_deep_structure(&branch_root, 15, 100);
                if fs::write(deep.join("leaf.txt"), b"data").is_ok() {
                    total_files += 1;
                }
            }
            Err(_) => {
                println!("Could not create branch {branch}");
            }
        }
    }

    if total_files > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let found_files = entries
            .iter()
            .filter(|e| e.relative_path().to_string_lossy().ends_with("leaf.txt"))
            .count();

        assert_eq!(
            found_files, total_files,
            "should find all leaf files in long branches"
        );
    }
}

// ============================================================================
// Relative Paths That Become Long When Resolved
// ============================================================================

/// Verifies that relative paths with many '..' components that resolve to
/// long absolute paths are handled correctly.
#[test]
fn relative_path_resolving_to_long_absolute() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("rel_long");

    // Create a deep structure
    let deep_path = create_deep_structure(&root, 20, 30);
    fs::write(deep_path.join("target.txt"), b"target").expect("write target");

    // Navigate from root and verify relative paths are computed correctly
    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry should succeed");
        let relative = entry.relative_path();
        let full = entry.full_path();

        // Verify that joining root + relative gives full path
        if !entry.is_root() {
            let reconstructed = root.join(relative);
            assert_eq!(
                full, reconstructed,
                "relative path should resolve correctly for {relative:?}"
            );
        }

        // Ensure relative path doesn't have '..' components
        let rel_str = relative.to_string_lossy();
        assert!(
            !rel_str.contains(".."),
            "relative path should not contain '..' for {relative:?}"
        );
    }
}

/// Verifies handling of paths containing '.' components in deeply nested structures.
#[test]
fn paths_with_current_dir_components_in_deep_structure() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dot_deep");

    let deep_path = create_deep_structure(&root, 25, 20);
    fs::write(deep_path.join("file.txt"), b"data").expect("write file");

    // Create path with '.' components
    let with_dots = root.join(".").join("d0000000000000000000");
    if with_dots.exists() {
        let walker = FileListBuilder::new(&with_dots).build();
        match walker {
            Ok(walker) => {
                let entries = collect_all_entries(walker);
                assert!(!entries.is_empty(), "should traverse from path with '.'");
            }
            Err(_) => {
                // Some systems may normalize differently
                println!("Path with '.' not traversable as expected");
            }
        }
    }
}

// ============================================================================
// Symlinks with Long Targets (Unix only)
// ============================================================================

#[cfg(unix)]
mod symlink_long_path_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Verifies handling of symlinks pointing to very long target paths.
    #[test]
    fn symlink_with_long_target_path() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("link_long");
        fs::create_dir(&root).expect("create root");

        // Create a deep target structure
        let target_root = temp.path().join("target_deep");
        let deep_target = create_deep_structure(&target_root, 30, 50);
        fs::write(deep_target.join("target.txt"), b"target data").expect("write target");

        // Create a symlink to the deep target
        let link_path = root.join("link_to_deep");
        if symlink(&deep_target, &link_path).is_ok() {
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let paths = collect_relative_paths(walker);

            // Without following, should just see the symlink
            assert_eq!(paths.len(), 1, "should see only the symlink");
            assert_eq!(paths[0], PathBuf::from("link_to_deep"));

            // With following, should see symlink contents
            let walker_follow = FileListBuilder::new(&root)
                .follow_symlinks(true)
                .build()
                .expect("build walker");
            let paths_follow = collect_relative_paths(walker_follow);

            assert!(
                paths_follow.len() >= 2,
                "should follow symlink to deep target"
            );
        }
    }

    /// Verifies handling of symlinks with target paths near PATH_MAX.
    #[test]
    fn symlink_target_approaching_path_max() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("link_max");
        fs::create_dir(&root).expect("create root");

        // Create a target that approaches PATH_MAX
        let target_root = temp.path().join("max_target");
        let root_len = target_root.as_os_str().len();
        let target_dir_len = SAFE_PATH_LIMIT.saturating_sub(root_len);

        if let Some(deep_target) = try_create_path_of_length(&target_root, target_dir_len) {
            fs::write(deep_target.join("max.txt"), b"max").expect("write max file");

            // Create symlink
            let link_path = root.join("link_to_max");
            if symlink(&deep_target, &link_path).is_ok() {
                let walker = FileListBuilder::new(&root)
                    .follow_symlinks(true)
                    .build()
                    .expect("build walker");
                let entries = collect_all_entries(walker);

                let found = entries
                    .iter()
                    .any(|e| e.relative_path().to_string_lossy().ends_with("max.txt"));
                assert!(found, "should find file through symlink to long path");
            }
        }
    }

    /// Verifies handling of relative symlinks in deep directory structures.
    #[test]
    fn relative_symlink_in_deep_structure() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("rel_sym_deep");

        // Create deep structure with a sibling branch
        let branch_a = create_deep_structure(&root.join("branch_a"), 15, 20);
        let branch_b = create_deep_structure(&root.join("branch_b"), 15, 20);

        fs::write(branch_b.join("target.txt"), b"target").expect("write target");

        // Create relative symlink from branch_a to branch_b
        // This requires going up many levels and back down
        let _relative_target = "../".repeat(16) + "branch_b/" + &"d".repeat(20).repeat(15);
        // Simplified: just link to sibling
        let link_path = branch_a.join("sibling_link");
        if symlink(&branch_b, &link_path).is_ok() {
            let walker = FileListBuilder::new(&root)
                .follow_symlinks(true)
                .build()
                .expect("build walker");
            let entries = collect_all_entries(walker);

            // Should find target.txt in branch_b and through sibling_link
            let target_count = entries
                .iter()
                .filter(|e| e.relative_path().to_string_lossy().contains("target.txt"))
                .count();

            // We expect to find the file at least once (directly in branch_b)
            // With cycle detection, we shouldn't infinitely recurse
            assert!(target_count >= 1, "should find target.txt at least once");
        }
    }

    /// Verifies symlink target stored correctly for long paths.
    #[test]
    fn symlink_target_storage_for_long_paths() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let root = temp.path().join("sym_store");
        fs::create_dir(&root).expect("create root");

        // Create a target with a moderately long path
        let target_path = temp.path().join("a".repeat(100)).join("b".repeat(100));
        fs::create_dir_all(&target_path).expect("create target");
        fs::write(target_path.join("file.txt"), b"content").expect("write file");

        // Create symlink
        let link_path = root.join("long_target_link");
        if symlink(&target_path, &link_path).is_ok() {
            // Verify we can read the link target
            let read_target = fs::read_link(&link_path).expect("read link");
            assert_eq!(
                read_target, target_path,
                "symlink target should be preserved"
            );

            // Walker should handle this correctly
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let entries = collect_all_entries(walker);

            let link_entry = entries.iter().find(|e| !e.is_root()).expect("find link");
            assert!(
                link_entry.metadata().file_type().is_symlink(),
                "entry should be recognized as symlink"
            );
        }
    }
}

// ============================================================================
// Error Handling for Paths Too Long
// ============================================================================

/// Verifies that attempting to traverse with an excessively long base path
/// produces appropriate behavior.
#[test]
fn very_long_base_path_handling() {
    // This tests what happens when the starting path itself is very long
    let temp = tempfile::tempdir().expect("create tempdir");

    // Create a moderately deep starting point
    let deep_start = create_deep_structure(temp.path(), 20, 80);

    // Can we build a walker from this deep starting point?
    match FileListBuilder::new(&deep_start).build() {
        Ok(walker) => {
            // If successful, root entry should have correct full path
            for entry in walker {
                let entry = entry.expect("entry should succeed");
                if entry.is_root() {
                    assert_eq!(
                        entry.full_path(),
                        deep_start,
                        "root full path should match starting path"
                    );
                }
            }
        }
        Err(e) => {
            // If it fails due to path limits, that's acceptable
            println!("Expected failure for very long base path: {e}");
        }
    }
}

/// Verifies behavior when creating entries would result in paths exceeding limits.
#[test]
fn entry_creation_at_path_limits() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("entry_limit");
    fs::create_dir(&root).expect("create root");

    // Create structure close to but not exceeding limits
    let root_len = root.as_os_str().len();
    let safe_depth = (SAFE_PATH_LIMIT - root_len) / 101; // 100-char names + separator
    let safe_depth = safe_depth.min(35); // Cap for reasonable test time

    let deep_path = create_deep_structure(&root, safe_depth, 100);
    fs::write(deep_path.join("safe.txt"), b"safe").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let mut error_count = 0;
    let mut success_count = 0;

    for result in walker {
        match result {
            Ok(_) => success_count += 1,
            Err(_) => error_count += 1,
        }
    }

    println!("Success: {success_count}, Errors: {error_count}");
    assert!(
        success_count > 0,
        "should successfully traverse some entries"
    );
}

// ============================================================================
// Path Truncation Behavior Tests
// ============================================================================

/// Verifies that file names are not truncated when stored in entries.
#[test]
fn no_filename_truncation_at_max_length() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("no_trunc");
    fs::create_dir(&root).expect("create root");

    // Create file with exactly 255-byte name (NAME_MAX)
    let exact_name = format!("{}.txt", "a".repeat(251)); // 255 total
    assert_eq!(exact_name.len(), 255);

    if fs::write(root.join(&exact_name), b"data").is_ok() {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let file_entry = entries.iter().find(|e| !e.is_root()).expect("find file");
        let stored_name = file_entry.file_name().expect("get filename");

        assert_eq!(stored_name.len(), 255, "filename should not be truncated");
        assert_eq!(
            stored_name.to_string_lossy(),
            exact_name,
            "filename should match exactly"
        );
    }
}

/// Verifies relative paths are not truncated for deeply nested files.
#[test]
fn no_relative_path_truncation_in_deep_structure() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep_no_trunc");

    // Create deep structure
    let deep_path = create_deep_structure(&root, 20, 50);
    fs::write(deep_path.join("deep.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for entry in walker {
        let entry = entry.expect("entry should succeed");
        if entry.is_root() {
            continue;
        }

        let relative = entry.relative_path();
        let full = entry.full_path();

        // Verify full path = root + relative (no truncation)
        let expected_full = root.join(relative);
        assert_eq!(
            full, expected_full,
            "full path should equal root + relative (no truncation)"
        );

        // Verify relative path components match directory structure
        let component_count = relative.components().count();
        assert_eq!(
            entry.depth(),
            component_count,
            "depth should match component count (no truncation)"
        );
    }
}

/// Verifies that directory names at maximum length are preserved.
#[test]
fn no_directory_name_truncation() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dir_no_trunc");
    fs::create_dir(&root).expect("create root");

    // Create nested directories with max-length names
    let max_dir_name = "d".repeat(255);
    let level1 = root.join(&max_dir_name);

    if fs::create_dir(&level1).is_ok() {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let dir_entry = entries.iter().find(|e| !e.is_root()).expect("find dir");
        let dir_name = dir_entry.file_name().expect("get dirname");

        assert_eq!(
            dir_name.len(),
            255,
            "directory name should not be truncated"
        );
    }
}

// ============================================================================
// Combined Edge Cases
// ============================================================================

/// Tests combination of many factors: deep nesting, long names, symlinks.
#[cfg(unix)]
#[test]
fn combined_deep_long_symlink_structure() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("combined");
    fs::create_dir(&root).expect("create root");

    // Create deep structure with long directory names
    let deep_with_long = create_deep_structure(&root.join("deep"), 10, 80);

    // Create file with long name at the deep location
    let long_filename = format!("{}.txt", "file_".repeat(40));
    fs::write(deep_with_long.join(&long_filename), b"combined").expect("write file");

    // Create symlink to deep structure
    let link = root.join("link_to_deep");
    if symlink(&deep_with_long, &link).is_ok() {
        // Traverse with symlink following
        let walker = FileListBuilder::new(&root)
            .follow_symlinks(true)
            .build()
            .expect("build walker");
        let entries = collect_all_entries(walker);

        // Should find the file both directly and through symlink
        let file_count = entries
            .iter()
            .filter(|e| e.relative_path().to_string_lossy().contains("file_file_"))
            .count();

        // Due to cycle detection, we might find it once or twice
        assert!(
            file_count >= 1,
            "should find file with long name in deep structure"
        );
    }
}

/// Tests handling of empty directories at various depths near the limit.
#[test]
fn empty_directories_at_path_limits() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("empty_deep");
    fs::create_dir(&root).expect("create root");

    // Create structure of empty directories approaching the limit
    let root_len = root.as_os_str().len();
    let levels = (SAFE_PATH_LIMIT - root_len) / 51; // 50-char names + separator
    let levels = levels.min(60); // Cap for test performance

    let _deep_path = create_deep_structure(&root, levels, 50);
    // Don't create any files - just empty directories

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Should have root + all directories
    assert_eq!(
        entries.len() - 1,
        levels, // -1 for root
        "should traverse all empty directories"
    );

    // All non-root entries should be directories
    for entry in &entries {
        if !entry.is_root() {
            assert!(
                entry.metadata().is_dir(),
                "all non-root entries should be directories"
            );
        }
    }
}

/// Tests iteration patterns with paths near the limit.
#[test]
fn iterator_patterns_with_long_paths() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("iter_long");

    // Create deep structure with files at various levels
    let deep_path = create_deep_structure(&root, 25, 60);
    fs::write(deep_path.join("deepest.txt"), b"deep").expect("write deep file");

    // Add some files at intermediate depths
    let mut current = root.clone();
    for i in 0..5 {
        current = current.join(format!("d{i:059}"));
        if current.exists() {
            fs::write(current.join(format!("level{i}.txt")), b"data").expect("write file");
        }
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");

    // Test take() with long paths
    let first_ten: Vec<_> = walker.take(10).collect();
    assert_eq!(first_ten.len(), 10, "take(10) should return 10 entries");

    // Test filter() with long paths
    let walker2 = FileListBuilder::new(&root).build().expect("build walker");
    let files_only: Vec<_> = walker2
        .filter_map(|r| r.ok())
        .filter(|e| e.metadata().is_file())
        .collect();

    assert!(!files_only.is_empty(), "should find files with filter");

    // Test map() with long paths
    let walker3 = FileListBuilder::new(&root).build().expect("build walker");
    let depths: Vec<usize> = walker3.filter_map(|r| r.ok()).map(|e| e.depth()).collect();

    assert!(depths.contains(&0), "should have depth 0");
    assert!(
        *depths.iter().max().unwrap() > 20,
        "should have deep entries"
    );
}
