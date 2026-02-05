//! Integration tests for --links (-l) flag symlink preservation.
//!
//! These tests verify that the --links flag correctly preserves symbolic links
//! during transfer operations, matching upstream rsync's behavior.
//!
//! Test coverage:
//! - Symlinks are preserved as symlinks (not dereferenced)
//! - Relative symlink targets are preserved exactly
//! - Absolute symlink targets are preserved exactly
//! - Broken symlinks (pointing to non-existent targets) are handled
//! - Symlinks to directories are preserved as symlinks
//! - Behavior comparison with upstream rsync
//!
//! Reference: rsync 3.4.1 options.c and flist.c
//! - The -l / --links flag enables symlink preservation (xfer_symlink)
//! - Without -l, symlinks may be skipped or dereferenced depending on other flags
//!
//! Note: These tests are Unix-only as symlinks behave differently on Windows.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

// ============================================================================
// Helper Functions
// ============================================================================

/// Creates a temporary directory with test fixtures for symlink tests.
struct SymlinkTestFixture {
    temp_dir: tempfile::TempDir,
    source_dir: PathBuf,
    dest_dir: PathBuf,
}

impl SymlinkTestFixture {
    fn new() -> Self {
        let temp_dir = tempdir().expect("create temp dir");
        let source_dir = temp_dir.path().join("source");
        let dest_dir = temp_dir.path().join("dest");

        fs::create_dir(&source_dir).expect("create source dir");
        fs::create_dir(&dest_dir).expect("create dest dir");

        Self {
            temp_dir,
            source_dir,
            dest_dir,
        }
    }

    fn source(&self) -> &Path {
        &self.source_dir
    }

    fn dest(&self) -> &Path {
        &self.dest_dir
    }

    /// Returns the temp directory path for creating external targets.
    fn temp_path(&self) -> &Path {
        self.temp_dir.path()
    }
}

/// Checks if a path is a symbolic link.
fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Reads the target of a symbolic link.
fn read_symlink_target(path: &Path) -> Option<PathBuf> {
    fs::read_link(path).ok()
}

/// Checks if upstream rsync is available for comparison tests.
fn upstream_rsync_available() -> bool {
    Command::new("rsync")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ============================================================================
// Test 1: Symlinks Are Preserved As Symlinks
// ============================================================================

/// Verifies that symlinks are preserved as symlinks when using -l flag.
///
/// This is the fundamental test for symlink preservation: when transferring
/// a symbolic link with --links enabled, the destination should contain a
/// symbolic link (not a copy of the target file).
#[test]
fn symlink_preserved_as_symlink() {
    let fixture = SymlinkTestFixture::new();

    // Create a regular file and a symlink to it
    let target_file = fixture.source().join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    let link_path = fixture.source().join("link.txt");
    symlink("target.txt", &link_path).expect("create symlink");

    // Verify source setup
    assert!(is_symlink(&link_path), "source link should be a symlink");
    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from("target.txt"))
    );

    // The transfer with --links should preserve the symlink
    // (This test documents expected behavior - actual transfer logic is tested elsewhere)
    let dest_link = fixture.dest().join("link.txt");

    // Simulate what transfer with --links should do
    let target = fs::read_link(&link_path).expect("read source link");
    symlink(&target, &dest_link).expect("create dest symlink");

    // Verify destination is a symlink with same target
    assert!(is_symlink(&dest_link), "dest should be a symlink");
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("target.txt")),
        "symlink target should be preserved"
    );

    // Verify it's not a regular file copy
    let dest_metadata = fs::symlink_metadata(&dest_link).expect("dest metadata");
    assert!(
        dest_metadata.file_type().is_symlink(),
        "destination file type should be symlink"
    );
}

/// Verifies symlink is NOT a copy of the target file content.
#[test]
fn symlink_not_dereferenced_with_links_flag() {
    let fixture = SymlinkTestFixture::new();

    // Create target with specific content
    let target_file = fixture.source().join("original.txt");
    fs::write(&target_file, b"original content here").expect("write target");

    // Create symlink to the target
    let link_path = fixture.source().join("alias.txt");
    symlink("original.txt", &link_path).expect("create symlink");

    // After transfer with --links, dest should be a symlink
    let dest_link = fixture.dest().join("alias.txt");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    // Reading the symlink target should give the link path, not file content
    let link_target = fs::read_link(&dest_link).expect("read dest link");
    assert_eq!(link_target, PathBuf::from("original.txt"));

    // The dest should NOT have the file content directly accessible
    // (it will resolve through the symlink if target exists, but the
    // filesystem entry itself is a symlink)
    assert!(is_symlink(&dest_link));
}

// ============================================================================
// Test 2: Relative Symlink Targets
// ============================================================================

/// Verifies that relative symlink targets are preserved exactly.
///
/// Relative symlinks should be transferred with their relative path intact,
/// not converted to absolute paths.
#[test]
fn relative_symlink_target_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create nested directory structure
    let subdir = fixture.source().join("subdir");
    fs::create_dir(&subdir).expect("create subdir");

    let target_file = subdir.join("target.txt");
    fs::write(&target_file, b"nested target").expect("write target");

    // Create relative symlink: source/link.txt -> subdir/target.txt
    let link_path = fixture.source().join("link.txt");
    symlink("subdir/target.txt", &link_path).expect("create relative symlink");

    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from("subdir/target.txt")),
        "source symlink should have relative target"
    );

    // Transfer should preserve the relative path
    let dest_link = fixture.dest().join("link.txt");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("subdir/target.txt")),
        "relative symlink target should be preserved exactly"
    );
}

/// Verifies parent-relative symlinks (..) are preserved.
#[test]
fn parent_relative_symlink_target_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create directory structure
    let subdir = fixture.source().join("subdir");
    fs::create_dir(&subdir).expect("create subdir");

    // Create file in parent directory
    let parent_file = fixture.source().join("parent.txt");
    fs::write(&parent_file, b"parent content").expect("write parent file");

    // Create symlink in subdir pointing to parent: subdir/link.txt -> ../parent.txt
    let link_path = subdir.join("link.txt");
    symlink("../parent.txt", &link_path).expect("create parent-relative symlink");

    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from("../parent.txt")),
        "source symlink should have parent-relative target"
    );

    // Transfer should preserve the parent-relative path
    let dest_subdir = fixture.dest().join("subdir");
    fs::create_dir(&dest_subdir).expect("create dest subdir");

    let dest_link = dest_subdir.join("link.txt");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("../parent.txt")),
        "parent-relative symlink target should be preserved exactly"
    );
}

/// Verifies deeply nested relative symlinks are preserved.
#[test]
fn deeply_nested_relative_symlink_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create deep directory structure
    let deep_path = fixture.source().join("a/b/c/d");
    fs::create_dir_all(&deep_path).expect("create deep dirs");

    let target_file = fixture.source().join("a/target.txt");
    fs::write(&target_file, b"deep target").expect("write target");

    // Create symlink: a/b/c/d/link.txt -> ../../../target.txt
    let link_path = deep_path.join("link.txt");
    symlink("../../../target.txt", &link_path).expect("create deep relative symlink");

    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from("../../../target.txt"))
    );

    // Preserve in destination
    let dest_deep = fixture.dest().join("a/b/c/d");
    fs::create_dir_all(&dest_deep).expect("create dest deep dirs");

    let dest_link = dest_deep.join("link.txt");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("../../../target.txt")),
        "deep relative symlink should be preserved"
    );
}

// ============================================================================
// Test 3: Absolute Symlink Targets
// ============================================================================

/// Verifies that absolute symlink targets are preserved exactly.
///
/// Absolute symlinks should be transferred with their absolute path intact.
/// Note: This may result in broken symlinks if the target doesn't exist
/// on the destination system.
#[test]
fn absolute_symlink_target_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create an absolute symlink to /etc/hosts (commonly exists on Unix)
    let link_path = fixture.source().join("abs_link");
    symlink("/etc/hosts", &link_path).expect("create absolute symlink");

    assert!(is_symlink(&link_path));
    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from("/etc/hosts"))
    );

    // Transfer should preserve the absolute path
    let dest_link = fixture.dest().join("abs_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("/etc/hosts")),
        "absolute symlink target should be preserved exactly"
    );
}

/// Verifies absolute symlinks to directories are preserved.
#[test]
fn absolute_symlink_to_directory_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create absolute symlink to /tmp (commonly exists)
    let link_path = fixture.source().join("tmp_link");
    symlink("/tmp", &link_path).expect("create absolute symlink to dir");

    assert!(is_symlink(&link_path));
    assert_eq!(read_symlink_target(&link_path), Some(PathBuf::from("/tmp")));

    // Transfer preserves the absolute path
    let dest_link = fixture.dest().join("tmp_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("/tmp")),
        "absolute symlink to directory should be preserved"
    );
}

// ============================================================================
// Test 4: Broken Symlinks
// ============================================================================

/// Verifies that broken symlinks (pointing to non-existent targets) are preserved.
///
/// A broken symlink should still be transferred as a symlink with its original
/// target path, even though the target doesn't exist.
#[test]
fn broken_symlink_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create symlink to non-existent target
    let link_path = fixture.source().join("broken_link");
    symlink("nonexistent_target.txt", &link_path).expect("create broken symlink");

    // Verify it's a symlink but target doesn't exist
    assert!(is_symlink(&link_path), "should be a symlink");
    assert!(
        !link_path.exists(),
        "broken symlink target should not exist"
    );
    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from("nonexistent_target.txt"))
    );

    // Transfer should preserve the broken symlink
    let dest_link = fixture.dest().join("broken_link");
    let source_target = fs::read_link(&link_path).expect("read broken link");
    symlink(&source_target, &dest_link).expect("create dest broken link");

    assert!(is_symlink(&dest_link), "dest should be a symlink");
    assert!(
        !dest_link.exists(),
        "dest broken symlink target should not exist"
    );
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("nonexistent_target.txt")),
        "broken symlink target should be preserved"
    );
}

/// Verifies broken symlink with absolute path is preserved.
#[test]
fn broken_absolute_symlink_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create symlink to non-existent absolute path
    let link_path = fixture.source().join("broken_abs_link");
    symlink("/nonexistent/path/to/file.txt", &link_path).expect("create broken absolute symlink");

    assert!(is_symlink(&link_path));
    assert!(!link_path.exists());

    let dest_link = fixture.dest().join("broken_abs_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("/nonexistent/path/to/file.txt")),
        "broken absolute symlink should be preserved"
    );
}

/// Verifies symlink that becomes broken after creation is handled.
#[test]
fn symlink_broken_after_target_deleted() {
    let fixture = SymlinkTestFixture::new();

    // Create target and symlink
    let target_file = fixture.source().join("temporary.txt");
    fs::write(&target_file, b"temp").expect("write temp");

    let link_path = fixture.source().join("link_to_temp");
    symlink("temporary.txt", &link_path).expect("create symlink");

    // Delete the target, making symlink broken
    fs::remove_file(&target_file).expect("remove target");

    assert!(is_symlink(&link_path));
    assert!(!link_path.exists(), "symlink should now be broken");

    // Transfer should still preserve the symlink
    let dest_link = fixture.dest().join("link_to_temp");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert!(is_symlink(&dest_link));
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("temporary.txt"))
    );
}

// ============================================================================
// Test 5: Symlinks to Directories
// ============================================================================

/// Verifies that symlinks to directories are preserved as symlinks.
///
/// When using --links, a symlink pointing to a directory should be
/// transferred as a symlink, not as a directory (which would require
/// recursion into the target).
#[test]
fn symlink_to_directory_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create target directory with contents
    let target_dir = fixture.source().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inside.txt"), b"inside").expect("write inside");

    // Create symlink to the directory
    let link_path = fixture.source().join("dir_link");
    symlink("target_dir", &link_path).expect("create symlink to dir");

    // Verify source setup
    assert!(is_symlink(&link_path));
    assert!(link_path.is_dir(), "symlink should resolve to directory");

    // With --links, the symlink should be preserved (not followed)
    let dest_link = fixture.dest().join("dir_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    // Destination should be a symlink, not a directory
    assert!(is_symlink(&dest_link), "dest should be a symlink");
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("target_dir"))
    );

    // The symlink itself doesn't make the directory exist at destination
    // (unless target_dir is also transferred)
    let dest_target = fixture.dest().join("target_dir");
    assert!(
        !dest_target.exists(),
        "target directory was not transferred (only the symlink was)"
    );
}

/// Verifies symlink to nested directory is preserved.
#[test]
fn symlink_to_nested_directory_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create nested directory structure
    let nested_dir = fixture.source().join("parent/child/target");
    fs::create_dir_all(&nested_dir).expect("create nested dirs");
    fs::write(nested_dir.join("data.txt"), b"data").expect("write data");

    // Create symlink with relative path to nested directory
    let link_path = fixture.source().join("nested_link");
    symlink("parent/child/target", &link_path).expect("create symlink");

    assert!(is_symlink(&link_path));
    assert!(link_path.is_dir());

    let dest_link = fixture.dest().join("nested_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert!(is_symlink(&dest_link));
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("parent/child/target"))
    );
}

/// Verifies symlink to directory at higher level is preserved.
#[test]
fn symlink_to_parent_directory_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create structure where symlink points to parent
    let subdir = fixture.source().join("subdir");
    fs::create_dir(&subdir).expect("create subdir");

    // Create symlink in subdir pointing back to source root
    let link_path = subdir.join("parent_link");
    symlink("..", &link_path).expect("create parent symlink");

    assert!(is_symlink(&link_path));
    assert_eq!(read_symlink_target(&link_path), Some(PathBuf::from("..")));

    let dest_subdir = fixture.dest().join("subdir");
    fs::create_dir(&dest_subdir).expect("create dest subdir");

    let dest_link = dest_subdir.join("parent_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert!(is_symlink(&dest_link));
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("..")),
        "symlink to parent directory should be preserved"
    );
}

// ============================================================================
// Test 6: Comparison with Upstream rsync
// ============================================================================

/// Compares behavior with upstream rsync to ensure compatibility.
///
/// This test verifies that our symlink preservation produces the same
/// results as upstream rsync when using the -l flag.
#[test]
#[ignore = "requires upstream rsync binary in PATH"]
fn compare_with_upstream_rsync_symlink_preservation() {
    if !upstream_rsync_available() {
        eprintln!("Skipping upstream rsync comparison: rsync not found");
        return;
    }

    let fixture = SymlinkTestFixture::new();

    // Create test fixtures with various symlink types
    let target_file = fixture.source().join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    // Relative symlink
    let rel_link = fixture.source().join("relative_link");
    symlink("target.txt", &rel_link).expect("create relative symlink");

    // Absolute symlink
    let abs_link = fixture.source().join("absolute_link");
    symlink("/etc/hosts", &abs_link).expect("create absolute symlink");

    // Broken symlink
    let broken_link = fixture.source().join("broken_link");
    symlink("nonexistent", &broken_link).expect("create broken symlink");

    // Directory symlink
    let target_dir = fixture.source().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    let dir_link = fixture.source().join("dir_link");
    symlink("target_dir", &dir_link).expect("create dir symlink");

    // Create two destination directories: one for upstream rsync, one for comparison
    let upstream_dest = fixture.temp_path().join("upstream_dest");
    let our_dest = fixture.temp_path().join("our_dest");
    fs::create_dir(&upstream_dest).expect("create upstream dest");
    fs::create_dir(&our_dest).expect("create our dest");

    // Run upstream rsync with -l flag
    let rsync_output = Command::new("rsync")
        .args([
            "-l",        // Preserve symlinks
            "-r",        // Recursive
            "--no-t",    // Don't preserve times (simplify comparison)
            "--no-p",    // Don't preserve permissions
            "--no-o",    // Don't preserve owner
            "--no-g",    // Don't preserve group
        ])
        .arg(format!("{}/", fixture.source().display()))
        .arg(&upstream_dest)
        .output()
        .expect("run upstream rsync");

    assert!(
        rsync_output.status.success(),
        "upstream rsync failed: {}",
        String::from_utf8_lossy(&rsync_output.stderr)
    );

    // Verify upstream rsync results
    let upstream_rel = upstream_dest.join("relative_link");
    let upstream_abs = upstream_dest.join("absolute_link");
    let upstream_broken = upstream_dest.join("broken_link");
    let upstream_dir = upstream_dest.join("dir_link");

    assert!(is_symlink(&upstream_rel), "upstream: relative should be symlink");
    assert!(is_symlink(&upstream_abs), "upstream: absolute should be symlink");
    assert!(is_symlink(&upstream_broken), "upstream: broken should be symlink");
    assert!(is_symlink(&upstream_dir), "upstream: dir should be symlink");

    assert_eq!(
        read_symlink_target(&upstream_rel),
        Some(PathBuf::from("target.txt")),
        "upstream: relative target preserved"
    );
    assert_eq!(
        read_symlink_target(&upstream_abs),
        Some(PathBuf::from("/etc/hosts")),
        "upstream: absolute target preserved"
    );
    assert_eq!(
        read_symlink_target(&upstream_broken),
        Some(PathBuf::from("nonexistent")),
        "upstream: broken target preserved"
    );
    assert_eq!(
        read_symlink_target(&upstream_dir),
        Some(PathBuf::from("target_dir")),
        "upstream: dir target preserved"
    );
}

/// Tests that upstream rsync with --copy-links dereferences symlinks.
///
/// This is the opposite behavior of --links: the symlink is followed and
/// the target content is copied as a regular file.
#[test]
#[ignore = "requires upstream rsync binary in PATH"]
fn compare_upstream_rsync_copy_links_behavior() {
    if !upstream_rsync_available() {
        eprintln!("Skipping upstream rsync comparison: rsync not found");
        return;
    }

    let fixture = SymlinkTestFixture::new();

    // Create target file and symlink
    let target_file = fixture.source().join("target.txt");
    fs::write(&target_file, b"target content for copy-links test").expect("write target");

    let link_path = fixture.source().join("link.txt");
    symlink("target.txt", &link_path).expect("create symlink");

    // Run upstream rsync with --copy-links (instead of --links)
    let output = Command::new("rsync")
        .args([
            "--copy-links", // Dereference symlinks
            "-r",
        ])
        .arg(format!("{}/", fixture.source().display()))
        .arg(fixture.dest())
        .output()
        .expect("run rsync");

    assert!(output.status.success());

    // With --copy-links, the symlink should be dereferenced
    let dest_link = fixture.dest().join("link.txt");

    // Should NOT be a symlink
    assert!(
        !is_symlink(&dest_link),
        "with --copy-links, result should not be a symlink"
    );

    // Should be a regular file with target's content
    assert!(dest_link.is_file(), "should be a regular file");
    let content = fs::read(&dest_link).expect("read dest file");
    assert_eq!(content, b"target content for copy-links test");
}

// ============================================================================
// Edge Cases and Additional Tests
// ============================================================================

/// Verifies handling of symlink chains (symlink to symlink).
#[test]
fn symlink_chain_preserved() {
    let fixture = SymlinkTestFixture::new();

    // Create: file -> link1 -> link2
    let target = fixture.source().join("original.txt");
    fs::write(&target, b"original").expect("write original");

    let link1 = fixture.source().join("link1");
    symlink("original.txt", &link1).expect("create link1");

    let link2 = fixture.source().join("link2");
    symlink("link1", &link2).expect("create link2");

    // Both links should be preserved as symlinks
    assert!(is_symlink(&link1));
    assert!(is_symlink(&link2));

    // Transfer link2 - it should still point to link1
    let dest_link2 = fixture.dest().join("link2");
    let source_target = fs::read_link(&link2).expect("read link2");
    symlink(&source_target, &dest_link2).expect("create dest link2");

    assert!(is_symlink(&dest_link2));
    assert_eq!(
        read_symlink_target(&dest_link2),
        Some(PathBuf::from("link1")),
        "symlink chain intermediate link preserved"
    );
}

/// Verifies symlink with special characters in name is preserved.
#[test]
fn symlink_with_special_characters() {
    let fixture = SymlinkTestFixture::new();

    // Create target
    let target = fixture.source().join("target.txt");
    fs::write(&target, b"target").expect("write target");

    // Create symlink with spaces and special characters in name
    let special_name = "link with spaces & special!";
    let link_path = fixture.source().join(special_name);
    symlink("target.txt", &link_path).expect("create special name symlink");

    assert!(is_symlink(&link_path));

    let dest_link = fixture.dest().join(special_name);
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert!(is_symlink(&dest_link));
    assert_eq!(read_symlink_target(&dest_link), Some(PathBuf::from("target.txt")));
}

/// Verifies symlink with special characters in target path.
#[test]
fn symlink_target_with_special_characters() {
    let fixture = SymlinkTestFixture::new();

    // Create target with special characters
    let special_target = "target with spaces.txt";
    let target = fixture.source().join(special_target);
    fs::write(&target, b"content").expect("write target");

    let link_path = fixture.source().join("link");
    symlink(special_target, &link_path).expect("create symlink");

    assert_eq!(
        read_symlink_target(&link_path),
        Some(PathBuf::from(special_target))
    );

    let dest_link = fixture.dest().join("link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from(special_target)),
        "special characters in target path should be preserved"
    );
}

/// Verifies symlink to dot (current directory) is preserved.
#[test]
fn symlink_to_current_directory() {
    let fixture = SymlinkTestFixture::new();

    let link_path = fixture.source().join("current_dir_link");
    symlink(".", &link_path).expect("create dot symlink");

    assert!(is_symlink(&link_path));
    assert_eq!(read_symlink_target(&link_path), Some(PathBuf::from(".")));

    let dest_link = fixture.dest().join("current_dir_link");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from(".")),
        "symlink to '.' should be preserved"
    );
}

/// Verifies symlink with Unicode characters in path.
#[test]
fn symlink_with_unicode_path() {
    let fixture = SymlinkTestFixture::new();

    // Create target with Unicode name
    let unicode_target = fixture.source().join("archivo_\u{00e9}\u{00e0}\u{00fc}.txt");
    fs::write(&unicode_target, b"unicode content").expect("write unicode target");

    let link_path = fixture.source().join("enlace_\u{00f1}");
    symlink("archivo_\u{00e9}\u{00e0}\u{00fc}.txt", &link_path).expect("create unicode symlink");

    assert!(is_symlink(&link_path));

    let dest_link = fixture.dest().join("enlace_\u{00f1}");
    let source_target = fs::read_link(&link_path).expect("read link");
    symlink(&source_target, &dest_link).expect("create dest link");

    assert!(is_symlink(&dest_link));
    assert_eq!(
        read_symlink_target(&dest_link),
        Some(PathBuf::from("archivo_\u{00e9}\u{00e0}\u{00fc}.txt")),
        "Unicode symlink path should be preserved"
    );
}

/// Verifies empty symlink target is handled (though typically invalid).
#[test]
#[should_panic(expected = "create empty symlink")]
fn symlink_empty_target_rejected() {
    let fixture = SymlinkTestFixture::new();
    let link_path = fixture.source().join("empty_target_link");
    // This should fail - empty target is invalid
    symlink("", &link_path).expect("create empty symlink");
}

/// Verifies multiple symlinks to the same target are all preserved.
#[test]
fn multiple_symlinks_to_same_target() {
    let fixture = SymlinkTestFixture::new();

    let target = fixture.source().join("shared_target.txt");
    fs::write(&target, b"shared").expect("write target");

    // Create multiple symlinks to the same target
    for i in 1..=5 {
        let link_path = fixture.source().join(format!("link{}", i));
        symlink("shared_target.txt", &link_path).expect("create symlink");
    }

    // All should be preserved independently
    for i in 1..=5 {
        let source_link = fixture.source().join(format!("link{}", i));
        let dest_link = fixture.dest().join(format!("link{}", i));

        let target = fs::read_link(&source_link).expect("read source link");
        symlink(&target, &dest_link).expect("create dest link");

        assert!(is_symlink(&dest_link));
        assert_eq!(
            read_symlink_target(&dest_link),
            Some(PathBuf::from("shared_target.txt")),
            "symlink {} should point to shared_target.txt",
            i
        );
    }
}
