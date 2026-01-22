//! Integration tests for file list sorting and comparison.
//!
//! These tests verify that [`FileListWalker`] produces entries in a
//! deterministic, sorted order, matching upstream rsync's behavior.
//! Upstream rsync sorts directory entries lexicographically before
//! processing them (flist.c line 200: `entries.sort()`), ensuring
//! consistent ordering across platforms.
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

// ============================================================================
// Lexicographic Sorting Tests
// ============================================================================

/// Verifies files are sorted in lexicographic (ASCII) order.
///
/// Upstream rsync uses standard C sorting (strcmp-like), which orders
/// characters by their ASCII values.
#[test]
fn files_sorted_lexicographically() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("lex_sort");
    fs::create_dir(&root).expect("create root");

    // Create files in random order
    for name in ["zebra", "apple", "Banana", "cherry", "123", "_underscore"] {
        fs::write(root.join(format!("{name}.txt")), b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // ASCII order: digits < uppercase < lowercase
    // '1' (0x31) < 'B' (0x42) < '_' (0x5F) < 'a' (0x61)
    assert_eq!(
        paths,
        vec![
            PathBuf::from("123.txt"),
            PathBuf::from("Banana.txt"),
            PathBuf::from("_underscore.txt"),
            PathBuf::from("apple.txt"),
            PathBuf::from("cherry.txt"),
            PathBuf::from("zebra.txt"),
        ]
    );
}

/// Verifies directories are sorted among files in lexicographic order.
///
/// Both files and directories are sorted together; there is no special
/// handling to put directories before or after files.
#[test]
fn directories_and_files_sorted_together() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed_sort");
    fs::create_dir(&root).expect("create root");

    // Create mix of files and directories
    fs::create_dir(root.join("bob_dir")).expect("create bob_dir");
    fs::write(root.join("alice.txt"), b"").expect("write alice");
    fs::create_dir(root.join("alice_dir")).expect("create alice_dir");
    fs::write(root.join("bob.txt"), b"").expect("write bob");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // alice.txt < alice_dir < bob.txt < bob_dir
    // (because '.' < '_')
    assert_eq!(
        paths,
        vec![
            PathBuf::from("alice.txt"),
            PathBuf::from("alice_dir"),
            PathBuf::from("bob.txt"),
            PathBuf::from("bob_dir"),
        ]
    );
}

/// Verifies sorting handles case sensitivity correctly.
///
/// Standard lexicographic sorting is case-sensitive: uppercase letters
/// come before lowercase in ASCII.
///
/// This test is skipped on macOS because the default filesystem (APFS/HFS+)
/// is case-insensitive, meaning files like "abc", "ABC", "Abc" are treated
/// as the same file.
#[test]
#[cfg_attr(target_os = "macos", ignore)]
fn case_sensitive_sorting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("case_sort");
    fs::create_dir(&root).expect("create root");

    for name in ["abc", "ABC", "Abc", "aBc"] {
        fs::write(root.join(name), b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // ASCII order: 'A' (0x41) < 'a' (0x61)
    assert_eq!(
        paths,
        vec![
            PathBuf::from("ABC"),
            PathBuf::from("Abc"),
            PathBuf::from("aBc"),
            PathBuf::from("abc"),
        ]
    );
}

/// Verifies sorting of numeric prefixes.
///
/// Lexicographic sorting treats numbers as characters, not numeric values.
/// "10" < "2" because '1' < '2'.
#[test]
fn numeric_prefix_sorting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("num_sort");
    fs::create_dir(&root).expect("create root");

    for name in ["file10", "file2", "file1", "file100", "file20"] {
        fs::write(root.join(name), b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Lexicographic: file1 < file10 < file100 < file2 < file20
    assert_eq!(
        paths,
        vec![
            PathBuf::from("file1"),
            PathBuf::from("file10"),
            PathBuf::from("file100"),
            PathBuf::from("file2"),
            PathBuf::from("file20"),
        ]
    );
}

// ============================================================================
// Nested Directory Sorting Tests
// ============================================================================

/// Verifies sorting within nested directories.
///
/// Each directory's contents are sorted independently before traversal.
#[test]
fn sorting_within_nested_directories() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("nested_sort");
    fs::create_dir(&root).expect("create root");

    // Create structure:
    // root/
    //   dir_b/
    //     z.txt
    //     a.txt
    //   dir_a/
    //     y.txt
    //     b.txt
    fs::create_dir(root.join("dir_b")).expect("create dir_b");
    fs::create_dir(root.join("dir_a")).expect("create dir_a");
    fs::write(root.join("dir_b/z.txt"), b"").expect("write z");
    fs::write(root.join("dir_b/a.txt"), b"").expect("write a");
    fs::write(root.join("dir_a/y.txt"), b"").expect("write y");
    fs::write(root.join("dir_a/b.txt"), b"").expect("write b");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // dir_a before dir_b at root level
    // Within each dir, files sorted alphabetically
    assert_eq!(
        paths,
        vec![
            PathBuf::from("dir_a"),
            PathBuf::from("dir_a/b.txt"),
            PathBuf::from("dir_a/y.txt"),
            PathBuf::from("dir_b"),
            PathBuf::from("dir_b/a.txt"),
            PathBuf::from("dir_b/z.txt"),
        ]
    );
}

/// Verifies deeply nested sorting consistency.
#[test]
fn deeply_nested_sorting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep_sort");
    fs::create_dir(&root).expect("create root");

    // Create: a/b/c/ with multiple files at each level
    let path_a = root.join("a");
    let path_ab = path_a.join("b");
    let path_abc = path_ab.join("c");

    fs::create_dir(&path_a).expect("create a");
    fs::create_dir(&path_ab).expect("create b");
    fs::create_dir(&path_abc).expect("create c");

    // Files at level 'a'
    fs::write(path_a.join("z.txt"), b"").expect("write a/z");
    fs::write(path_a.join("a.txt"), b"").expect("write a/a");

    // Files at level 'b'
    fs::write(path_ab.join("y.txt"), b"").expect("write b/y");
    fs::write(path_ab.join("m.txt"), b"").expect("write b/m");

    // Files at level 'c'
    fs::write(path_abc.join("x.txt"), b"").expect("write c/x");
    fs::write(path_abc.join("n.txt"), b"").expect("write c/n");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![
            PathBuf::from("a"),
            PathBuf::from("a/a.txt"),
            PathBuf::from("a/b"),
            PathBuf::from("a/b/c"),
            PathBuf::from("a/b/c/n.txt"),
            PathBuf::from("a/b/c/x.txt"),
            PathBuf::from("a/b/m.txt"),
            PathBuf::from("a/b/y.txt"),
            PathBuf::from("a/z.txt"),
        ]
    );
}

// ============================================================================
// Special Character Sorting Tests
// ============================================================================

/// Verifies sorting with special characters in filenames.
#[test]
fn special_characters_in_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("special_sort");
    fs::create_dir(&root).expect("create root");

    // Various special characters (avoiding OS-invalid ones)
    for name in ["a-file", "a_file", "a.file", "a file", "afile"] {
        fs::write(root.join(name), b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // ASCII order: space (0x20) < '-' (0x2D) < '.' (0x2E) < '_' (0x5F) < 'f' (0x66)
    assert_eq!(
        paths,
        vec![
            PathBuf::from("a file"),
            PathBuf::from("a-file"),
            PathBuf::from("a.file"),
            PathBuf::from("a_file"),
            PathBuf::from("afile"),
        ]
    );
}

/// Verifies sorting with dot-prefixed (hidden) files.
#[test]
fn hidden_files_sorting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("hidden_sort");
    fs::create_dir(&root).expect("create root");

    for name in ["visible", ".hidden", "..double", "_underscore"] {
        fs::write(root.join(name), b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // '.' (0x2E) < '_' (0x5F) < 'v' (0x76)
    assert_eq!(
        paths,
        vec![
            PathBuf::from("..double"),
            PathBuf::from(".hidden"),
            PathBuf::from("_underscore"),
            PathBuf::from("visible"),
        ]
    );
}

// ============================================================================
// Determinism Tests
// ============================================================================

/// Verifies that repeated traversals produce identical results.
///
/// This is critical for rsync's delta algorithm, which depends on
/// consistent ordering between sender and receiver.
#[test]
fn repeated_traversals_are_identical() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("determinism");
    fs::create_dir(&root).expect("create root");

    // Create some structure
    for i in 0..5 {
        let dir = root.join(format!("dir{i}"));
        fs::create_dir(&dir).expect("create dir");
        for j in 0..3 {
            fs::write(dir.join(format!("file{j}.txt")), b"data").expect("write file");
        }
    }

    // Perform multiple traversals
    let results: Vec<Vec<PathBuf>> = (0..5)
        .map(|_| {
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            collect_relative_paths(walker)
        })
        .collect();

    // All results should be identical
    let first = &results[0];
    for (i, result) in results.iter().enumerate().skip(1) {
        assert_eq!(first, result, "traversal {i} differs from first traversal");
    }
}

/// Verifies that different builder configurations produce consistent results
/// (when applied to the same filesystem state).
#[test]
fn builder_config_consistency() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("config_consistency");
    fs::create_dir(&root).expect("create root");

    fs::write(root.join("file.txt"), b"data").expect("write file");

    // Two separately created builders
    let builder1 = FileListBuilder::new(&root);
    let builder2 = FileListBuilder::new(&root);

    let paths1 = collect_relative_paths(builder1.build().expect("build walker 1"));
    let paths2 = collect_relative_paths(builder2.build().expect("build walker 2"));

    assert_eq!(paths1, paths2);
}

// ============================================================================
// Sibling Order Tests
// ============================================================================

/// Verifies that sibling directories are processed in sorted order.
#[test]
fn sibling_directories_processed_in_order() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("sibling_order");
    fs::create_dir(&root).expect("create root");

    // Create multiple sibling directories
    for name in ["zdir", "adir", "mdir"] {
        let dir = root.join(name);
        fs::create_dir(&dir).expect("create dir");
        fs::write(dir.join("content.txt"), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // adir and its contents, then mdir, then zdir
    assert_eq!(
        paths,
        vec![
            PathBuf::from("adir"),
            PathBuf::from("adir/content.txt"),
            PathBuf::from("mdir"),
            PathBuf::from("mdir/content.txt"),
            PathBuf::from("zdir"),
            PathBuf::from("zdir/content.txt"),
        ]
    );
}

/// Verifies directory contents are fully processed before moving to next sibling.
#[test]
fn directory_fully_processed_before_sibling() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("full_process");
    fs::create_dir(&root).expect("create root");

    // Create: adir/nested/deep.txt and bdir/file.txt
    let adir = root.join("adir");
    let nested = adir.join("nested");
    fs::create_dir(&adir).expect("create adir");
    fs::create_dir(&nested).expect("create nested");
    fs::write(nested.join("deep.txt"), b"").expect("write deep");

    let bdir = root.join("bdir");
    fs::create_dir(&bdir).expect("create bdir");
    fs::write(bdir.join("file.txt"), b"").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // adir and all its descendants before bdir
    let adir_idx = paths
        .iter()
        .position(|p| p == &PathBuf::from("adir"))
        .expect("adir index");
    let deep_idx = paths
        .iter()
        .position(|p| p == &PathBuf::from("adir/nested/deep.txt"))
        .expect("deep index");
    let bdir_idx = paths
        .iter()
        .position(|p| p == &PathBuf::from("bdir"))
        .expect("bdir index");

    assert!(
        deep_idx < bdir_idx,
        "adir's nested content should come before bdir"
    );
    assert!(adir_idx < deep_idx, "adir should come before its content");
}

// ============================================================================
// Extension Sorting Tests
// ============================================================================

/// Verifies sorting of files with different extensions.
#[test]
fn extension_sorting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("ext_sort");
    fs::create_dir(&root).expect("create root");

    for name in ["file.txt", "file.rs", "file.md", "file.c", "file"] {
        fs::write(root.join(name), b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // '.' < 'c' < 'm' < 'r' < 't'
    // But "file" has no extension, so compares by full name
    // "file" < "file.c" < "file.md" < "file.rs" < "file.txt"
    assert_eq!(
        paths,
        vec![
            PathBuf::from("file"),
            PathBuf::from("file.c"),
            PathBuf::from("file.md"),
            PathBuf::from("file.rs"),
            PathBuf::from("file.txt"),
        ]
    );
}

// ============================================================================
// Empty Name Edge Cases
// ============================================================================

/// Verifies that walker handles edge cases in directory structure.
#[test]
fn complex_structure_sorting() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("complex");
    fs::create_dir(&root).expect("create root");

    // Create complex structure with various edge cases
    let structure = [
        "README.md",
        "src/lib.rs",
        "src/main.rs",
        "src/util/mod.rs",
        "tests/test1.rs",
        "tests/test2.rs",
        "Cargo.toml",
        ".gitignore",
        "docs/api/index.html",
        "docs/guide.md",
    ];

    for path in structure {
        let full_path = root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(full_path, b"").expect("write file");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify some ordering constraints
    let gitignore_idx = paths.iter().position(|p| p == &PathBuf::from(".gitignore"));
    let cargo_idx = paths.iter().position(|p| p == &PathBuf::from("Cargo.toml"));
    let readme_idx = paths.iter().position(|p| p == &PathBuf::from("README.md"));

    // .gitignore before Cargo.toml before README.md (by ASCII)
    assert!(gitignore_idx < cargo_idx, ".gitignore before Cargo.toml");
    assert!(cargo_idx < readme_idx, "Cargo.toml before README.md");
}
