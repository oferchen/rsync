//! Tests for cross-device hardlink handling edge cases.

use std::path::PathBuf;

use super::super::unix::HardLinkKey;

/// Test that the same inode number on different devices produces different keys.
#[test]
fn same_inode_different_device() {
    let key1 = HardLinkKey {
        device: 1,
        inode: 12345,
    };
    let key2 = HardLinkKey {
        device: 2,
        inode: 12345,
    };

    assert_ne!(
        key1, key2,
        "same inode on different devices should be different"
    );

    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(key1);
    assert!(!set.contains(&key2));
}

/// Test tracking files that happen to have the same inode number.
///
/// On different filesystems, files can have the same inode number.
/// The device number distinguishes them.
#[test]
fn different_devices_tracked_separately() {
    // Simulate two files on different devices with same inode
    // by directly testing key behavior
    let key_dev1 = HardLinkKey {
        device: 0xFD00,
        inode: 100,
    };
    let key_dev2 = HardLinkKey {
        device: 0xFD01,
        inode: 100,
    };

    let mut entries = rustc_hash::FxHashMap::default();
    entries.insert(key_dev1, PathBuf::from("/mnt/disk1/file.txt"));
    entries.insert(key_dev2, PathBuf::from("/mnt/disk2/file.txt"));

    // Both should be tracked separately
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries.get(&key_dev1),
        Some(&PathBuf::from("/mnt/disk1/file.txt"))
    );
    assert_eq!(
        entries.get(&key_dev2),
        Some(&PathBuf::from("/mnt/disk2/file.txt"))
    );
}
