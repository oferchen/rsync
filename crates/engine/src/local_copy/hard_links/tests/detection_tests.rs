//! Tests for detection of hard-linked files based on inode information.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::local_copy::hard_links::HardLinkTracker;
use crate::local_copy::test_support;

/// Test that files with nlink > 1 are detected as hard links.
#[test]
fn detects_hard_linked_files_by_nlink() {
    let temp = test_support::create_tempdir();
    let file1 = temp.path().join("file1.txt");
    let file2 = temp.path().join("file2.txt");

    std::fs::write(&file1, "shared content").unwrap();
    std::fs::hard_link(&file1, &file2).unwrap();

    let metadata1 = std::fs::metadata(&file1).unwrap();
    let metadata2 = std::fs::metadata(&file2).unwrap();

    assert_eq!(metadata1.nlink(), 2, "first file should have nlink=2");
    assert_eq!(metadata2.nlink(), 2, "second file should have nlink=2");

    let key1 = HardLinkTracker::key(&metadata1);
    let key2 = HardLinkTracker::key(&metadata2);

    assert!(key1.is_some(), "should generate key for hardlinked file");
    assert!(key2.is_some(), "should generate key for hardlinked file");

    // Keys should be equal (same device/inode)
    assert_eq!(
        key1.unwrap(),
        key2.unwrap(),
        "hardlinked files should have same key"
    );
}

/// Test that standalone files (nlink = 1) are not tracked.
#[test]
fn ignores_standalone_files_with_nlink_one() {
    let temp = test_support::create_tempdir();
    let file = temp.path().join("standalone.txt");
    std::fs::write(&file, "standalone content").unwrap();

    let metadata = std::fs::metadata(&file).unwrap();
    assert_eq!(metadata.nlink(), 1, "standalone file should have nlink=1");

    let key = HardLinkTracker::key(&metadata);
    assert!(
        key.is_none(),
        "standalone files should not generate a tracking key"
    );
}

/// Test that the tracker correctly identifies first occurrence vs subsequent.
#[test]
fn tracker_identifies_first_and_subsequent_occurrences() {
    let temp = test_support::create_tempdir();
    let file1 = temp.path().join("first.txt");
    let file2 = temp.path().join("second.txt");
    let file3 = temp.path().join("third.txt");

    std::fs::write(&file1, "content").unwrap();
    std::fs::hard_link(&file1, &file2).unwrap();
    std::fs::hard_link(&file1, &file3).unwrap();

    let metadata1 = std::fs::metadata(&file1).unwrap();
    let metadata2 = std::fs::metadata(&file2).unwrap();
    let metadata3 = std::fs::metadata(&file3).unwrap();

    let mut tracker = HardLinkTracker::new();

    assert!(
        tracker.existing_target(&metadata1).is_none(),
        "first occurrence should not have existing target"
    );

    let dest1 = PathBuf::from("/dest/first.txt");
    tracker.record(&metadata1, &dest1);

    let existing = tracker.existing_target(&metadata2);
    assert!(existing.is_some(), "second occurrence should find target");
    assert_eq!(
        existing.unwrap(),
        dest1,
        "should return the first recorded destination"
    );

    // Record second file (overwrites entry)
    let dest2 = PathBuf::from("/dest/second.txt");
    tracker.record(&metadata2, &dest2);

    let existing = tracker.existing_target(&metadata3);
    assert!(existing.is_some(), "third occurrence should find target");
    assert_eq!(
        existing.unwrap(),
        dest2,
        "should return the most recent destination"
    );
}

/// Test that device and inode are both used in key generation.
#[test]
fn key_uses_both_device_and_inode() {
    use super::super::unix::HardLinkKey;

    let key_a = HardLinkKey {
        device: 1,
        inode: 100,
    };
    let key_b = HardLinkKey {
        device: 1,
        inode: 200,
    };
    let key_c = HardLinkKey {
        device: 2,
        inode: 100,
    };
    let key_d = HardLinkKey {
        device: 1,
        inode: 100,
    };

    // Same device, different inode
    assert_ne!(
        key_a, key_b,
        "different inodes should produce different keys"
    );

    // Different device, same inode
    assert_ne!(
        key_a, key_c,
        "different devices should produce different keys"
    );

    // Same device and inode
    assert_eq!(key_a, key_d, "same device/inode should produce equal keys");
}

/// Test tracking of multiple independent hardlink groups.
#[test]
fn tracks_multiple_independent_hardlink_groups() {
    let temp = test_support::create_tempdir();

    // Group 1: file1a and file1b
    let file1a = temp.path().join("group1_a.txt");
    let file1b = temp.path().join("group1_b.txt");
    std::fs::write(&file1a, "group1 content").unwrap();
    std::fs::hard_link(&file1a, &file1b).unwrap();

    // Group 2: file2a and file2b (different content/inode)
    let file2a = temp.path().join("group2_a.txt");
    let file2b = temp.path().join("group2_b.txt");
    std::fs::write(&file2a, "group2 content").unwrap();
    std::fs::hard_link(&file2a, &file2b).unwrap();

    let meta1a = std::fs::metadata(&file1a).unwrap();
    let meta1b = std::fs::metadata(&file1b).unwrap();
    let meta2a = std::fs::metadata(&file2a).unwrap();
    let meta2b = std::fs::metadata(&file2b).unwrap();

    assert_ne!(
        meta1a.ino(),
        meta2a.ino(),
        "different groups should have different inodes"
    );

    let mut tracker = HardLinkTracker::new();

    let dest1 = PathBuf::from("/dest/group1_a.txt");
    tracker.record(&meta1a, &dest1);

    let dest2 = PathBuf::from("/dest/group2_a.txt");
    tracker.record(&meta2a, &dest2);

    let existing1 = tracker.existing_target(&meta1b);
    assert_eq!(
        existing1,
        Some(dest1.clone()),
        "group1 member should link to group1 destination"
    );

    let existing2 = tracker.existing_target(&meta2b);
    assert_eq!(
        existing2,
        Some(dest2),
        "group2 member should link to group2 destination"
    );
}

/// Test that recording a standalone file (nlink=1) does nothing.
#[test]
fn recording_standalone_file_has_no_effect() {
    let temp = test_support::create_tempdir();
    let file = temp.path().join("standalone.txt");
    std::fs::write(&file, "content").unwrap();

    let metadata = std::fs::metadata(&file).unwrap();
    assert_eq!(metadata.nlink(), 1);

    let mut tracker = HardLinkTracker::new();
    tracker.record(&metadata, Path::new("/dest/standalone.txt"));

    assert!(
        tracker.existing_target(&metadata).is_none(),
        "standalone file should not be tracked"
    );
}
