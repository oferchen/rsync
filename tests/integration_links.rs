//! Integration tests for symlinks, hard links, and special files.
//!
//! Tests link handling, preservation, and special file types.

mod integration;

#[cfg(unix)]
use integration::helpers::*;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix;

// ============================================================================
// Symlink Tests
// ============================================================================

#[test]
#[cfg(unix)]
fn symlinks_preserved_with_links_flag() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a regular file and a symlink to it
    fs::write(src_dir.join("target.txt"), b"target content").unwrap();
    unix::fs::symlink("target.txt", src_dir.join("link.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Both file and symlink should exist
    assert!(dest_dir.join("target.txt").exists());
    assert!(dest_dir.join("link.txt").exists());

    // Verify it's actually a symlink
    let metadata = fs::symlink_metadata(dest_dir.join("link.txt")).unwrap();
    assert!(metadata.is_symlink(), "link.txt should be a symlink");

    // Verify the symlink points to the right target
    let link_target = fs::read_link(dest_dir.join("link.txt")).unwrap();
    assert_eq!(link_target.to_str().unwrap(), "target.txt");
}

#[test]
#[cfg(unix)]
fn copy_links_dereferences_symlinks() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("target.txt"), b"content").unwrap();
    unix::fs::symlink("target.txt", src_dir.join("link.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--copy-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Link should be copied as a regular file
    let metadata = fs::symlink_metadata(dest_dir.join("link.txt")).unwrap();
    assert!(!metadata.is_symlink(), "link.txt should be a regular file");

    // Content should match the target
    let content = fs::read(dest_dir.join("link.txt")).unwrap();
    assert_eq!(content, b"content");
}

#[test]
#[cfg(unix)]
fn symlinks_ignored_without_links_flag() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"regular").unwrap();
    unix::fs::symlink("file.txt", src_dir.join("link.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        // No --links flag
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Regular file should be copied
    assert!(dest_dir.join("file.txt").exists());

    // Symlink should be skipped (default behavior without -l)
    assert!(
        !dest_dir.join("link.txt").exists(),
        "symlinks should be skipped without --links"
    );
}

#[test]
#[cfg(unix)]
fn dangling_symlink_preserved() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a symlink to a non-existent target
    unix::fs::symlink("nonexistent.txt", src_dir.join("dangling.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Dangling symlink should still be preserved
    // Use symlink_metadata instead of exists() because exists() checks the target
    let metadata = fs::symlink_metadata(dest_dir.join("dangling.txt")).unwrap();
    assert!(
        metadata.is_symlink(),
        "dangling symlink should be preserved"
    );

    // Verify it points to the non-existent target
    let link_target = fs::read_link(dest_dir.join("dangling.txt")).unwrap();
    assert_eq!(link_target.to_str().unwrap(), "nonexistent.txt");
}

// ============================================================================
// Hard Link Tests
// ============================================================================

#[test]
#[cfg(unix)]
fn hard_links_preserved_with_hard_links_flag() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a file and a hard link to it
    fs::write(src_dir.join("original.txt"), b"shared content").unwrap();
    fs::hard_link(src_dir.join("original.txt"), src_dir.join("hardlink.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Both files should exist
    assert!(dest_dir.join("original.txt").exists());
    assert!(dest_dir.join("hardlink.txt").exists());

    // Verify they're actually hard linked (same inode)
    use std::os::unix::fs::MetadataExt;
    let orig_meta = fs::metadata(dest_dir.join("original.txt")).unwrap();
    let link_meta = fs::metadata(dest_dir.join("hardlink.txt")).unwrap();

    assert_eq!(
        orig_meta.ino(),
        link_meta.ino(),
        "files should share the same inode"
    );
    assert_eq!(
        orig_meta.nlink(),
        2,
        "link count should be 2 for hard linked files"
    );
}

#[test]
#[cfg(unix)]
fn hard_links_copied_separately_without_flag() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file1.txt"), b"content").unwrap();
    fs::hard_link(src_dir.join("file1.txt"), src_dir.join("file2.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        // No --hard-links flag
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Both files should exist with same content
    assert!(dest_dir.join("file1.txt").exists());
    assert!(dest_dir.join("file2.txt").exists());

    let content1 = fs::read(dest_dir.join("file1.txt")).unwrap();
    let content2 = fs::read(dest_dir.join("file2.txt")).unwrap();
    assert_eq!(content1, content2);

    // But they should have different inodes (separate copies)
    use std::os::unix::fs::MetadataExt;
    let meta1 = fs::metadata(dest_dir.join("file1.txt")).unwrap();
    let meta2 = fs::metadata(dest_dir.join("file2.txt")).unwrap();

    assert_ne!(
        meta1.ino(),
        meta2.ino(),
        "without --hard-links, files should have different inodes"
    );
}

// ============================================================================
// Archive Mode Link Handling
// ============================================================================

#[test]
#[cfg(unix)]
fn archive_mode_preserves_symlinks() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"content").unwrap();
    unix::fs::symlink("file.txt", src_dir.join("link.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a", // Archive mode includes --links
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Archive mode should preserve symlinks
    let metadata = fs::symlink_metadata(dest_dir.join("link.txt")).unwrap();
    assert!(
        metadata.is_symlink(),
        "archive mode should preserve symlinks"
    );
}

#[test]
#[cfg(unix)]
fn archive_with_hard_links_preserves_both() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create regular file, hard link, and symlink
    fs::write(src_dir.join("orig.txt"), b"content").unwrap();
    fs::hard_link(src_dir.join("orig.txt"), src_dir.join("hard.txt")).unwrap();
    unix::fs::symlink("orig.txt", src_dir.join("soft.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Verify symlink
    let soft_meta = fs::symlink_metadata(dest_dir.join("soft.txt")).unwrap();
    assert!(soft_meta.is_symlink());

    // Verify hard link
    use std::os::unix::fs::MetadataExt;
    let orig_meta = fs::metadata(dest_dir.join("orig.txt")).unwrap();
    let hard_meta = fs::metadata(dest_dir.join("hard.txt")).unwrap();
    assert_eq!(orig_meta.ino(), hard_meta.ino());
}
