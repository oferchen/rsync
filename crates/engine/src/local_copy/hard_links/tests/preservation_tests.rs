//! Tests for preservation behavior with the -H flag.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::local_copy::hard_links::HardLinkTracker;
use crate::local_copy::test_support;

/// Test that first file in a hardlink group is copied, subsequent are linked.
#[test]
fn first_file_copied_subsequent_linked() {
    let temp = test_support::create_tempdir();
    let files: Vec<_> = (0..5)
        .map(|i| temp.path().join(format!("file{i}.txt")))
        .collect();

    std::fs::write(&files[0], "shared content").unwrap();
    for file in &files[1..] {
        std::fs::hard_link(&files[0], file).unwrap();
    }

    let mut tracker = HardLinkTracker::new();

    for (idx, file) in files.iter().enumerate() {
        let metadata = std::fs::metadata(file).unwrap();
        let dest = PathBuf::from(format!("/dest/file{idx}.txt"));

        let existing = tracker.existing_target(&metadata);
        if idx == 0 {
            assert!(
                existing.is_none(),
                "first file should not have existing target"
            );
        } else {
            assert!(existing.is_some(), "file {idx} should have existing target");
        }

        tracker.record(&metadata, &dest);
    }
}

/// Test that different content files with nlink=1 remain separate.
#[test]
fn standalone_files_remain_separate() {
    let temp = test_support::create_tempdir();
    let file1 = temp.path().join("file1.txt");
    let file2 = temp.path().join("file2.txt");

    std::fs::write(&file1, "content1").unwrap();
    std::fs::write(&file2, "content2").unwrap();

    let meta1 = std::fs::metadata(&file1).unwrap();
    let meta2 = std::fs::metadata(&file2).unwrap();

    assert_eq!(meta1.nlink(), 1);
    assert_eq!(meta2.nlink(), 1);

    assert_ne!(meta1.ino(), meta2.ino());

    let mut tracker = HardLinkTracker::new();

    // Record both (should be no-op for nlink=1 files)
    tracker.record(&meta1, Path::new("/dest/file1.txt"));
    tracker.record(&meta2, Path::new("/dest/file2.txt"));

    assert!(tracker.existing_target(&meta1).is_none());
    assert!(tracker.existing_target(&meta2).is_none());
}

/// Test behavior when hardlink group has mixed nlink values during sync.
///
/// This can happen when some files in a group are deleted during transfer.
#[test]
fn handles_varying_nlink_values() {
    let temp = test_support::create_tempdir();
    let file1 = temp.path().join("file1.txt");
    let file2 = temp.path().join("file2.txt");
    let file3 = temp.path().join("file3.txt");

    std::fs::write(&file1, "content").unwrap();
    std::fs::hard_link(&file1, &file2).unwrap();
    std::fs::hard_link(&file1, &file3).unwrap();

    // All files now have nlink = 3
    let mut tracker = HardLinkTracker::new();

    let meta1 = std::fs::metadata(&file1).unwrap();
    tracker.record(&meta1, Path::new("/dest/file1.txt"));

    // Delete one link to change nlink
    std::fs::remove_file(&file3).unwrap();

    let meta2 = std::fs::metadata(&file2).unwrap();
    assert_eq!(meta2.nlink(), 2);

    // Should still find the existing target (same device/inode)
    let existing = tracker.existing_target(&meta2);
    assert!(
        existing.is_some(),
        "should find target even with changed nlink"
    );
}
