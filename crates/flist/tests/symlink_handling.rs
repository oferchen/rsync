//! Integration tests for symlink handling in file list traversal.
//!
//! These tests verify that [`FileListBuilder`] and [`FileListWalker`] correctly
//! handle symbolic links, matching upstream rsync's behavior for both
//! `--copy-links` (follow symlinks) and default (preserve symlinks) modes.
//!
//! Upstream rsync handles symlinks via:
//! - `readlink_stat()` in flist.c line 205-232
//! - `link_stat()` in flist.c line 234-250
//! - Cycle detection to prevent infinite loops
//!
//! Reference: rsync 3.4.1 flist.c
//!
//! Note: These tests are Unix-only as symlinks behave differently on Windows.

#![cfg(unix)]

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::fs;
use std::os::unix::fs::symlink;
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
// Default Symlink Behavior (No Following)
// ============================================================================

/// Verifies that symlinks are yielded but not followed by default.
///
/// By default, rsync preserves symbolic links (`-l` / `--links`). The walker
/// should emit the symlink entry but not descend into the target directory.
#[test]
fn symlink_to_directory_not_followed_by_default() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let target = temp.path().join("target");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("inside.txt"), b"data").expect("write inside");

    // Create symlink: root/link -> target
    symlink(&target, root.join("link")).expect("create symlink");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should only contain the symlink, not its contents
    assert_eq!(paths, vec![PathBuf::from("link")]);
}

/// Verifies symlink metadata indicates it is a symlink.
#[test]
fn symlink_metadata_is_symlink() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let target = temp.path().join("target");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target).expect("create target");

    symlink(&target, root.join("link")).expect("create symlink");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let link_entry = entries
        .iter()
        .find(|e| e.relative_path() == std::path::Path::new("link"))
        .expect("link entry");

    assert!(
        link_entry.metadata().file_type().is_symlink(),
        "entry metadata should indicate symlink"
    );
}

/// Verifies symlink to file is yielded without following.
#[test]
fn symlink_to_file_not_followed_by_default() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");

    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    symlink(&target_file, root.join("link.txt")).expect("create symlink");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let link_entry = entries
        .iter()
        .find(|e| e.relative_path() == std::path::Path::new("link.txt"))
        .expect("link entry");

    assert!(link_entry.metadata().file_type().is_symlink());
}

/// Verifies multiple symlinks in a directory are all yielded.
#[test]
fn multiple_symlinks_in_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let targets = temp.path().join("targets");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(&targets).expect("create targets");

    // Create multiple target directories
    for name in ["target_a", "target_b", "target_c"] {
        let target = targets.join(name);
        fs::create_dir(&target).expect("create target dir");
        fs::write(target.join("file.txt"), b"data").expect("write file");
    }

    // Create symlinks
    symlink(targets.join("target_a"), root.join("link_a")).expect("create link_a");
    symlink(targets.join("target_b"), root.join("link_b")).expect("create link_b");
    symlink(targets.join("target_c"), root.join("link_c")).expect("create link_c");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // All symlinks should be present, sorted
    assert_eq!(
        paths,
        vec![
            PathBuf::from("link_a"),
            PathBuf::from("link_b"),
            PathBuf::from("link_c"),
        ]
    );
}

// ============================================================================
// Follow Symlinks Behavior
// ============================================================================

/// Verifies that symlinks are followed when enabled.
///
/// With `follow_symlinks(true)`, the walker descends into symlinked
/// directories, similar to rsync's `--copy-links` option.
#[test]
fn symlink_to_directory_followed_when_enabled() {
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

    // Should contain both the symlink and its contents
    assert_eq!(
        paths,
        vec![PathBuf::from("link"), PathBuf::from("link/inside.txt")]
    );
}

/// Verifies that nested symlinks are followed when enabled.
#[test]
fn nested_symlinks_followed() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let target1 = temp.path().join("target1");
    let target2 = temp.path().join("target2");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target1).expect("create target1");
    fs::create_dir(&target2).expect("create target2");

    fs::write(target2.join("deep.txt"), b"deep").expect("write deep");

    // Create: root/link1 -> target1, target1/link2 -> target2
    symlink(&target1, root.join("link1")).expect("create link1");
    symlink(&target2, target1.join("link2")).expect("create link2");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths,
        vec![
            PathBuf::from("link1"),
            PathBuf::from("link1/link2"),
            PathBuf::from("link1/link2/deep.txt"),
        ]
    );
}

/// Verifies symlinked file contents are accessible when following.
#[test]
fn symlink_preserves_relative_paths_when_following() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let target = temp.path().join("target");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"content").expect("write file");

    symlink(&target, root.join("link")).expect("create symlink");

    let mut walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    // Skip root
    let _ = walker.next();

    // Get link entry
    let link_entry = walker.next().expect("link entry").expect("success");
    assert_eq!(link_entry.relative_path(), std::path::Path::new("link"));
    assert!(link_entry.metadata().file_type().is_symlink());

    // Get file inside link
    let file_entry = walker.next().expect("file entry").expect("success");
    assert_eq!(
        file_entry.relative_path(),
        std::path::Path::new("link/file.txt")
    );
    // The file inside is a regular file, not a symlink
    assert!(file_entry.metadata().is_file());
}

// ============================================================================
// Cycle Detection Tests
// ============================================================================

/// Verifies that symlink cycles are detected and prevented.
///
/// When a symlink points back to an ancestor directory, the walker must
/// detect this to prevent infinite loops. This matches upstream rsync's
/// behavior using canonical path tracking.
#[test]
fn symlink_cycle_to_self_detected() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");

    // Create symlink pointing to its own parent directory
    symlink(&root, root.join("self")).expect("create self-referencing symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should yield the symlink but not recurse into it
    assert_eq!(paths, vec![PathBuf::from("self")]);
}

/// Verifies that indirect cycles are detected.
#[test]
fn symlink_indirect_cycle_detected() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");

    // Create cycle: subdir/link -> root
    symlink(&root, root.join("subdir/link")).expect("create cycle symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should yield subdir and its link, but not re-enter root
    assert_eq!(
        paths,
        vec![PathBuf::from("subdir"), PathBuf::from("subdir/link")]
    );
}

/// Verifies that complex cycle patterns are handled.
#[test]
fn complex_symlink_cycle_detected() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    // Create: root/a/b/link -> root/a
    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("a")).expect("create a");
    fs::create_dir(root.join("a/b")).expect("create b");
    fs::write(root.join("a/file.txt"), b"data").expect("write file");

    symlink(root.join("a"), root.join("a/b/link")).expect("create cycle");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should visit a, a/b, a/b/link, a/file.txt but not re-enter a
    let expected: Vec<PathBuf> = vec![
        "a".into(),
        "a/b".into(),
        "a/b/link".into(),
        "a/file.txt".into(),
    ];

    assert_eq!(paths, expected);
}

// ============================================================================
// Broken Symlink Tests
// ============================================================================

/// Verifies that broken symlinks (pointing to non-existent targets) are handled.
///
/// A broken symlink still exists as a symlink entry; it just can't be
/// dereferenced. The walker should yield the symlink entry.
#[test]
fn broken_symlink_is_yielded() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");

    // Create symlink to non-existent target
    symlink("/nonexistent/path/target", root.join("broken")).expect("create broken symlink");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths, vec![PathBuf::from("broken")]);
}

/// Verifies broken symlink metadata indicates it is a symlink.
#[test]
fn broken_symlink_metadata_is_symlink() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");
    symlink("/nonexistent", root.join("broken")).expect("create broken symlink");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let broken_entry = entries
        .iter()
        .find(|e| e.relative_path() == std::path::Path::new("broken"))
        .expect("broken entry");

    assert!(broken_entry.metadata().file_type().is_symlink());
}

// ============================================================================
// Root Symlink Tests
// ============================================================================

/// Verifies behavior when the root itself is a symlink (not followed).
#[test]
fn root_is_symlink_not_followed() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let target = temp.path().join("target");
    let link = temp.path().join("link");

    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"data").expect("write file");

    symlink(&target, &link).expect("create root symlink");

    let mut walker = FileListBuilder::new(&link).build().expect("build walker");

    let root_entry = walker.next().expect("root entry").expect("success");
    assert!(root_entry.is_root());
    assert!(root_entry.metadata().file_type().is_symlink());

    // Should not descend into symlink target by default
    assert!(walker.next().is_none());
}

/// Verifies behavior when the root is a symlink and following is enabled.
#[test]
fn root_is_symlink_followed_when_enabled() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let target = temp.path().join("target");
    let link = temp.path().join("link");

    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"data").expect("write file");

    symlink(&target, &link).expect("create root symlink");

    let walker = FileListBuilder::new(&link)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let entries = collect_all_entries(walker);

    // Root should still be symlink in metadata
    assert!(entries[0].is_root());
    assert!(entries[0].metadata().file_type().is_symlink());

    // But should descend into target
    let paths: Vec<_> = entries
        .iter()
        .filter(|e| !e.is_root())
        .map(|e| e.relative_path().to_path_buf())
        .collect();
    assert_eq!(paths, vec![PathBuf::from("file.txt")]);
}

/// Verifies full_path for entries when root is a symlink.
#[test]
fn root_symlink_full_paths_use_link_path() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let target = temp.path().join("target");
    let link = temp.path().join("link");

    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"data").expect("write file");

    symlink(&target, &link).expect("create root symlink");

    let walker = FileListBuilder::new(&link)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let entries = collect_all_entries(walker);

    // Root full_path should be the link, not the target
    assert_eq!(entries[0].full_path(), link.as_path());

    // Child full_path should be link/file.txt, not target/file.txt
    let file_entry = entries.iter().find(|e| !e.is_root()).expect("file entry");
    assert_eq!(file_entry.full_path(), link.join("file.txt").as_path());
}

// ============================================================================
// Symlink Sorting Tests
// ============================================================================

/// Verifies symlinks are sorted alongside regular files and directories.
#[test]
fn symlinks_sorted_with_other_entries() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let target = temp.path().join("target");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target).expect("create target");

    // Create mix of files, directories, and symlinks
    fs::write(root.join("b_file.txt"), b"").expect("write file");
    fs::create_dir(root.join("c_dir")).expect("create dir");
    symlink(&target, root.join("a_link")).expect("create symlink");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // All should be sorted alphabetically
    assert_eq!(
        paths,
        vec![
            PathBuf::from("a_link"),
            PathBuf::from("b_file.txt"),
            PathBuf::from("c_dir"),
        ]
    );
}

// ============================================================================
// Relative Symlink Tests
// ============================================================================

/// Verifies relative symlinks are handled correctly.
#[test]
fn relative_symlink_in_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("target")).expect("create target");
    fs::write(root.join("target/file.txt"), b"data").expect("write file");

    // Create relative symlink: root/link -> target
    symlink("target", root.join("link")).expect("create relative symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should follow the relative symlink
    assert!(paths.contains(&PathBuf::from("link")));
    assert!(paths.contains(&PathBuf::from("link/file.txt")));
}

/// Verifies parent-relative symlinks (..) are handled.
#[test]
fn parent_relative_symlink() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    let external = temp.path().join("external");

    fs::create_dir(&root).expect("create root");
    fs::create_dir(root.join("subdir")).expect("create subdir");
    fs::create_dir(&external).expect("create external");
    fs::write(external.join("file.txt"), b"data").expect("write file");

    // Create symlink: root/subdir/link -> ../../external
    symlink("../../external", root.join("subdir/link")).expect("create parent-relative symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should follow the parent-relative symlink
    assert!(paths.contains(&PathBuf::from("subdir/link")));
    assert!(paths.contains(&PathBuf::from("subdir/link/file.txt")));
}

// ============================================================================
// Symlink to File Tests
// ============================================================================

/// Verifies symlinks to files are handled when following symlinks.
#[test]
fn symlink_to_file_when_following() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");

    fs::create_dir(&root).expect("create root");

    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    symlink(&target_file, root.join("link.txt")).expect("create symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // File symlinks don't have "contents" to descend into
    assert_eq!(paths, vec![PathBuf::from("link.txt")]);
}
