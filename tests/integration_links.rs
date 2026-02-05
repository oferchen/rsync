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

// ============================================================================
// Comprehensive Hard Link Preservation Tests
// ============================================================================

#[test]
#[cfg(unix)]
fn multiple_hardlinks_to_same_inode() {
    // Test that multiple hardlinks (3+) to the same inode are all preserved
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a file and multiple hard links to it
    fs::write(
        src_dir.join("original.txt"),
        b"shared content between all links",
    )
    .unwrap();
    fs::hard_link(src_dir.join("original.txt"), src_dir.join("link1.txt")).unwrap();
    fs::hard_link(src_dir.join("original.txt"), src_dir.join("link2.txt")).unwrap();
    fs::hard_link(src_dir.join("original.txt"), src_dir.join("link3.txt")).unwrap();

    // Verify source setup: all should have same inode and nlink=4
    use std::os::unix::fs::MetadataExt;
    let src_orig_ino = fs::metadata(src_dir.join("original.txt")).unwrap().ino();
    assert_eq!(
        fs::metadata(src_dir.join("link1.txt")).unwrap().ino(),
        src_orig_ino
    );
    assert_eq!(
        fs::metadata(src_dir.join("link2.txt")).unwrap().ino(),
        src_orig_ino
    );
    assert_eq!(
        fs::metadata(src_dir.join("link3.txt")).unwrap().ino(),
        src_orig_ino
    );
    assert_eq!(
        fs::metadata(src_dir.join("original.txt")).unwrap().nlink(),
        4
    );

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // All files should exist
    assert!(dest_dir.join("original.txt").exists());
    assert!(dest_dir.join("link1.txt").exists());
    assert!(dest_dir.join("link2.txt").exists());
    assert!(dest_dir.join("link3.txt").exists());

    // All should share the same inode
    let dest_orig_meta = fs::metadata(dest_dir.join("original.txt")).unwrap();
    let dest_link1_meta = fs::metadata(dest_dir.join("link1.txt")).unwrap();
    let dest_link2_meta = fs::metadata(dest_dir.join("link2.txt")).unwrap();
    let dest_link3_meta = fs::metadata(dest_dir.join("link3.txt")).unwrap();

    let dest_ino = dest_orig_meta.ino();
    assert_eq!(
        dest_link1_meta.ino(),
        dest_ino,
        "link1 should share inode with original"
    );
    assert_eq!(
        dest_link2_meta.ino(),
        dest_ino,
        "link2 should share inode with original"
    );
    assert_eq!(
        dest_link3_meta.ino(),
        dest_ino,
        "link3 should share inode with original"
    );

    // Link count should be 4
    assert_eq!(
        dest_orig_meta.nlink(),
        4,
        "all 4 files should be hard linked (nlink=4)"
    );

    // Content should be identical
    assert_eq!(
        fs::read(dest_dir.join("original.txt")).unwrap(),
        b"shared content between all links"
    );
    assert_eq!(
        fs::read(dest_dir.join("link1.txt")).unwrap(),
        b"shared content between all links"
    );
}

#[test]
#[cfg(unix)]
fn hardlinks_across_directories() {
    // Test that hardlinks across different directories are preserved
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create directory structure with hardlinks spanning directories
    fs::create_dir(src_dir.join("dir_a")).unwrap();
    fs::create_dir(src_dir.join("dir_b")).unwrap();
    fs::create_dir(src_dir.join("dir_c")).unwrap();

    // Create file in dir_a and hardlink to it from dir_b and dir_c
    fs::write(
        src_dir.join("dir_a/file.txt"),
        b"cross-directory linked content",
    )
    .unwrap();
    fs::hard_link(
        src_dir.join("dir_a/file.txt"),
        src_dir.join("dir_b/link.txt"),
    )
    .unwrap();
    fs::hard_link(
        src_dir.join("dir_a/file.txt"),
        src_dir.join("dir_c/another_link.txt"),
    )
    .unwrap();

    // Also create a file at root level linked to same inode
    fs::hard_link(
        src_dir.join("dir_a/file.txt"),
        src_dir.join("root_link.txt"),
    )
    .unwrap();

    use std::os::unix::fs::MetadataExt;
    // Verify source setup
    let src_ino = fs::metadata(src_dir.join("dir_a/file.txt")).unwrap().ino();
    assert_eq!(
        fs::metadata(src_dir.join("dir_b/link.txt")).unwrap().ino(),
        src_ino
    );
    assert_eq!(
        fs::metadata(src_dir.join("dir_c/another_link.txt"))
            .unwrap()
            .ino(),
        src_ino
    );
    assert_eq!(
        fs::metadata(src_dir.join("root_link.txt")).unwrap().ino(),
        src_ino
    );

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // All files and directories should exist
    assert!(dest_dir.join("dir_a/file.txt").exists());
    assert!(dest_dir.join("dir_b/link.txt").exists());
    assert!(dest_dir.join("dir_c/another_link.txt").exists());
    assert!(dest_dir.join("root_link.txt").exists());

    // All should share the same inode
    let dest_ino = fs::metadata(dest_dir.join("dir_a/file.txt")).unwrap().ino();
    assert_eq!(
        fs::metadata(dest_dir.join("dir_b/link.txt")).unwrap().ino(),
        dest_ino,
        "dir_b/link.txt should share inode with dir_a/file.txt"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("dir_c/another_link.txt"))
            .unwrap()
            .ino(),
        dest_ino,
        "dir_c/another_link.txt should share inode with dir_a/file.txt"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("root_link.txt")).unwrap().ino(),
        dest_ino,
        "root_link.txt should share inode with dir_a/file.txt"
    );

    // Link count should be 4
    assert_eq!(
        fs::metadata(dest_dir.join("dir_a/file.txt"))
            .unwrap()
            .nlink(),
        4,
        "cross-directory hardlinks should maintain link count"
    );
}

#[test]
#[cfg(unix)]
fn hardlink_preservation_with_archive_flag() {
    // Test that -a combined with -H properly preserves hardlinks
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a more complex structure with nested directories
    fs::create_dir_all(src_dir.join("deep/nested/dir")).unwrap();

    // Create files and hardlinks at various levels
    fs::write(src_dir.join("file1.txt"), b"content one").unwrap();
    fs::hard_link(src_dir.join("file1.txt"), src_dir.join("file1_link.txt")).unwrap();

    fs::write(src_dir.join("deep/file2.txt"), b"content two").unwrap();
    fs::hard_link(
        src_dir.join("deep/file2.txt"),
        src_dir.join("deep/nested/file2_link.txt"),
    )
    .unwrap();

    // Cross-level hardlink
    fs::hard_link(
        src_dir.join("file1.txt"),
        src_dir.join("deep/nested/dir/file1_deep_link.txt"),
    )
    .unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-aH", // Archive mode + hard links (combined short flags)
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    use std::os::unix::fs::MetadataExt;

    // Verify file1 group (3 links)
    let file1_ino = fs::metadata(dest_dir.join("file1.txt")).unwrap().ino();
    assert_eq!(
        fs::metadata(dest_dir.join("file1_link.txt")).unwrap().ino(),
        file1_ino,
        "file1_link should be hardlinked to file1"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("deep/nested/dir/file1_deep_link.txt"))
            .unwrap()
            .ino(),
        file1_ino,
        "deep file1 link should be hardlinked to file1"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("file1.txt")).unwrap().nlink(),
        3,
        "file1 group should have nlink=3"
    );

    // Verify file2 group (2 links)
    let file2_ino = fs::metadata(dest_dir.join("deep/file2.txt")).unwrap().ino();
    assert_eq!(
        fs::metadata(dest_dir.join("deep/nested/file2_link.txt"))
            .unwrap()
            .ino(),
        file2_ino,
        "file2_link should be hardlinked to file2"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("deep/file2.txt"))
            .unwrap()
            .nlink(),
        2,
        "file2 group should have nlink=2"
    );

    // Verify content
    assert_eq!(
        fs::read(dest_dir.join("file1.txt")).unwrap(),
        b"content one"
    );
    assert_eq!(
        fs::read(dest_dir.join("deep/file2.txt")).unwrap(),
        b"content two"
    );
}

#[test]
#[cfg(unix)]
fn inode_relationships_maintained_across_transfer() {
    // Verify that inode relationships are properly maintained
    // even when multiple independent hardlink groups exist
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Group A: 3 files linked together
    fs::write(src_dir.join("group_a_1.txt"), b"group A content").unwrap();
    fs::hard_link(src_dir.join("group_a_1.txt"), src_dir.join("group_a_2.txt")).unwrap();
    fs::hard_link(src_dir.join("group_a_1.txt"), src_dir.join("group_a_3.txt")).unwrap();

    // Group B: 2 files linked together
    fs::write(src_dir.join("group_b_1.txt"), b"group B content").unwrap();
    fs::hard_link(src_dir.join("group_b_1.txt"), src_dir.join("group_b_2.txt")).unwrap();

    // Standalone file (no hardlinks)
    fs::write(src_dir.join("standalone.txt"), b"standalone content").unwrap();

    use std::os::unix::fs::MetadataExt;

    // Record source inodes
    let src_group_a_ino = fs::metadata(src_dir.join("group_a_1.txt")).unwrap().ino();
    let src_group_b_ino = fs::metadata(src_dir.join("group_b_1.txt")).unwrap().ino();
    let src_standalone_ino = fs::metadata(src_dir.join("standalone.txt")).unwrap().ino();

    // Verify groups are distinct in source
    assert_ne!(src_group_a_ino, src_group_b_ino);
    assert_ne!(src_group_a_ino, src_standalone_ino);
    assert_ne!(src_group_b_ino, src_standalone_ino);

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Get destination inodes
    let dest_group_a_1_ino = fs::metadata(dest_dir.join("group_a_1.txt")).unwrap().ino();
    let dest_group_a_2_ino = fs::metadata(dest_dir.join("group_a_2.txt")).unwrap().ino();
    let dest_group_a_3_ino = fs::metadata(dest_dir.join("group_a_3.txt")).unwrap().ino();
    let dest_group_b_1_ino = fs::metadata(dest_dir.join("group_b_1.txt")).unwrap().ino();
    let dest_group_b_2_ino = fs::metadata(dest_dir.join("group_b_2.txt")).unwrap().ino();
    let dest_standalone_ino = fs::metadata(dest_dir.join("standalone.txt")).unwrap().ino();

    // Verify Group A members share the same inode
    assert_eq!(
        dest_group_a_1_ino, dest_group_a_2_ino,
        "group_a files should share inode"
    );
    assert_eq!(
        dest_group_a_1_ino, dest_group_a_3_ino,
        "group_a files should share inode"
    );

    // Verify Group B members share the same inode
    assert_eq!(
        dest_group_b_1_ino, dest_group_b_2_ino,
        "group_b files should share inode"
    );

    // Verify groups are distinct from each other
    assert_ne!(
        dest_group_a_1_ino, dest_group_b_1_ino,
        "different groups should have different inodes"
    );
    assert_ne!(
        dest_group_a_1_ino, dest_standalone_ino,
        "hardlinked and standalone files should have different inodes"
    );
    assert_ne!(
        dest_group_b_1_ino, dest_standalone_ino,
        "hardlinked and standalone files should have different inodes"
    );

    // Verify link counts
    assert_eq!(
        fs::metadata(dest_dir.join("group_a_1.txt"))
            .unwrap()
            .nlink(),
        3,
        "group_a should have nlink=3"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("group_b_1.txt"))
            .unwrap()
            .nlink(),
        2,
        "group_b should have nlink=2"
    );
    assert_eq!(
        fs::metadata(dest_dir.join("standalone.txt"))
            .unwrap()
            .nlink(),
        1,
        "standalone should have nlink=1"
    );

    // Verify content integrity
    assert_eq!(
        fs::read(dest_dir.join("group_a_1.txt")).unwrap(),
        b"group A content"
    );
    assert_eq!(
        fs::read(dest_dir.join("group_b_1.txt")).unwrap(),
        b"group B content"
    );
    assert_eq!(
        fs::read(dest_dir.join("standalone.txt")).unwrap(),
        b"standalone content"
    );
}

#[test]
#[cfg(unix)]
fn hardlinks_with_short_h_flag() {
    // Test that -H (short form) works the same as --hard-links
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("original.txt"), b"content").unwrap();
    fs::hard_link(src_dir.join("original.txt"), src_dir.join("hardlink.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-rH", // Recursive + hard links using short form
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    use std::os::unix::fs::MetadataExt;
    let orig_meta = fs::metadata(dest_dir.join("original.txt")).unwrap();
    let link_meta = fs::metadata(dest_dir.join("hardlink.txt")).unwrap();

    assert_eq!(
        orig_meta.ino(),
        link_meta.ino(),
        "-H flag should preserve hardlinks"
    );
    assert_eq!(orig_meta.nlink(), 2);
}

#[test]
#[cfg(unix)]
fn hardlinks_incremental_transfer() {
    // Test that hardlinks are preserved during incremental transfers
    // when some files already exist at destination
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source files with hardlinks
    fs::write(src_dir.join("file1.txt"), b"content").unwrap();
    fs::hard_link(src_dir.join("file1.txt"), src_dir.join("file2.txt")).unwrap();
    fs::hard_link(src_dir.join("file1.txt"), src_dir.join("file3.txt")).unwrap();

    // First transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    use std::os::unix::fs::MetadataExt;
    let _first_ino = fs::metadata(dest_dir.join("file1.txt")).unwrap().ino();

    // Add another hardlink to the source
    fs::hard_link(src_dir.join("file1.txt"), src_dir.join("file4.txt")).unwrap();

    // Second (incremental) transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // All 4 files should now be hardlinked at destination
    let dest_file1_ino = fs::metadata(dest_dir.join("file1.txt")).unwrap().ino();
    let dest_file2_ino = fs::metadata(dest_dir.join("file2.txt")).unwrap().ino();
    let dest_file3_ino = fs::metadata(dest_dir.join("file3.txt")).unwrap().ino();
    let dest_file4_ino = fs::metadata(dest_dir.join("file4.txt")).unwrap().ino();

    assert_eq!(dest_file1_ino, dest_file2_ino);
    assert_eq!(dest_file1_ino, dest_file3_ino);
    assert_eq!(dest_file1_ino, dest_file4_ino);
    assert_eq!(
        fs::metadata(dest_dir.join("file1.txt")).unwrap().nlink(),
        4,
        "all 4 files should be hardlinked after incremental transfer"
    );
}

#[test]
#[cfg(unix)]
fn hardlinks_modify_through_any_link() {
    // Verify that modifying content through any hardlink affects all links
    // (this is standard hardlink behavior, but we verify rsync preserves it)
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("original.txt"), b"initial content").unwrap();
    fs::hard_link(src_dir.join("original.txt"), src_dir.join("link.txt")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--hard-links",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Modify content through one link
    fs::write(dest_dir.join("link.txt"), b"modified content").unwrap();

    // Verify modification is visible through the other link
    assert_eq!(
        fs::read(dest_dir.join("original.txt")).unwrap(),
        b"modified content",
        "modification through one hardlink should be visible through other links"
    );
    assert_eq!(
        fs::read(dest_dir.join("link.txt")).unwrap(),
        b"modified content"
    );
}
