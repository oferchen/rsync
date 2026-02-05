//! Hard link tracking for local copy operations.
//!
//! Uses [`FxHashMap`] for fast lookups with integer-based device/inode keys.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rustc_hash::FxHashMap;

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct HardLinkTracker {
    entries: FxHashMap<HardLinkKey, PathBuf>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HardLinkKey {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl HardLinkTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn existing_target(&self, metadata: &fs::Metadata) -> Option<PathBuf> {
        Self::key(metadata).and_then(|key| self.entries.get(&key).cloned())
    }

    pub(crate) fn record(&mut self, metadata: &fs::Metadata, destination: &Path) {
        if let Some(key) = Self::key(metadata) {
            self.entries.insert(key, destination.to_path_buf());
        }
    }

    fn key(metadata: &fs::Metadata) -> Option<HardLinkKey> {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() > 1 {
            Some(HardLinkKey {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
#[derive(Default)]
pub(crate) struct HardLinkTracker;

#[cfg(not(unix))]
impl HardLinkTracker {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) fn existing_target(&self, _metadata: &fs::Metadata) -> Option<PathBuf> {
        None
    }

    pub(crate) fn record(&mut self, _metadata: &fs::Metadata, _destination: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_tracker() {
        let tracker = HardLinkTracker::new();
        let _ = tracker;
    }

    #[test]
    fn default_creates_tracker() {
        let tracker = HardLinkTracker::default();
        let _ = tracker;
    }

    #[test]
    fn existing_target_returns_none_for_new_tracker() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();
        let metadata = std::fs::metadata(&file).unwrap();

        let tracker = HardLinkTracker::new();
        assert!(tracker.existing_target(&metadata).is_none());
    }

    #[test]
    fn record_does_not_panic() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();
        let metadata = std::fs::metadata(&file).unwrap();

        let mut tracker = HardLinkTracker::new();
        tracker.record(&metadata, Path::new("/dest/test.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_eq() {
        let key1 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key2 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key3 = HardLinkKey {
            device: 2,
            inode: 100,
        };
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_hash() {
        use std::collections::HashSet;
        let key1 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key2 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let mut set = HashSet::new();
        set.insert(key1);
        assert!(set.contains(&key2));
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_debug() {
        let key = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let debug = format!("{key:?}");
        assert!(debug.contains("HardLinkKey"));
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_clone() {
        let key = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let cloned = key;
        assert_eq!(key, cloned);
    }
}

/// Tests for detection of hard-linked files based on inode information.
#[cfg(all(test, unix))]
mod detection_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Test that files with nlink > 1 are detected as hard links.
    #[test]
    fn detects_hard_linked_files_by_nlink() {
        let temp = tempfile::tempdir().unwrap();
        let file1 = temp.path().join("file1.txt");
        let file2 = temp.path().join("file2.txt");

        std::fs::write(&file1, "shared content").unwrap();
        std::fs::hard_link(&file1, &file2).unwrap();

        let metadata1 = std::fs::metadata(&file1).unwrap();
        let metadata2 = std::fs::metadata(&file2).unwrap();

        // Both files should have nlink = 2
        assert_eq!(metadata1.nlink(), 2, "first file should have nlink=2");
        assert_eq!(metadata2.nlink(), 2, "second file should have nlink=2");

        // The key function should return a key for both
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
        let temp = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
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

        // First occurrence: no existing target
        assert!(
            tracker.existing_target(&metadata1).is_none(),
            "first occurrence should not have existing target"
        );

        // Record the first file
        let dest1 = PathBuf::from("/dest/first.txt");
        tracker.record(&metadata1, &dest1);

        // Second occurrence: should find existing target
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

        // Third occurrence: should find the updated target
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
        let temp = tempfile::tempdir().unwrap();

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

        // Verify groups have different inodes
        assert_ne!(
            meta1a.ino(),
            meta2a.ino(),
            "different groups should have different inodes"
        );

        let mut tracker = HardLinkTracker::new();

        // Record first from group 1
        let dest1 = PathBuf::from("/dest/group1_a.txt");
        tracker.record(&meta1a, &dest1);

        // Record first from group 2
        let dest2 = PathBuf::from("/dest/group2_a.txt");
        tracker.record(&meta2a, &dest2);

        // Second from group 1 should link to group 1's destination
        let existing1 = tracker.existing_target(&meta1b);
        assert_eq!(
            existing1,
            Some(dest1.clone()),
            "group1 member should link to group1 destination"
        );

        // Second from group 2 should link to group 2's destination
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
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("standalone.txt");
        std::fs::write(&file, "content").unwrap();

        let metadata = std::fs::metadata(&file).unwrap();
        assert_eq!(metadata.nlink(), 1);

        let mut tracker = HardLinkTracker::new();
        tracker.record(&metadata, Path::new("/dest/standalone.txt"));

        // Query should still return None since standalone files are not tracked
        assert!(
            tracker.existing_target(&metadata).is_none(),
            "standalone file should not be tracked"
        );
    }
}

/// Tests for device/inode tracking edge cases.
#[cfg(all(test, unix))]
mod device_inode_tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// Test that extreme device/inode values are handled correctly.
    #[test]
    fn extreme_device_inode_values() {
        let test_cases = [
            (0u64, 0u64),
            (u64::MAX, u64::MAX),
            (u64::MAX, 0),
            (0, u64::MAX),
            (1, 1),
            (u64::MAX - 1, u64::MAX - 1),
        ];

        for (device, inode) in test_cases {
            let key = HardLinkKey { device, inode };
            // Should not panic
            let _ = format!("{key:?}");

            // Clone should work
            let cloned = key;
            assert_eq!(key, cloned);
        }
    }

    /// Test hash collision resistance for HardLinkKey.
    #[test]
    fn hash_collision_resistance() {
        fn hash_key(key: &HardLinkKey) -> u64 {
            let mut hasher = DefaultHasher::new();
            key.hash(&mut hasher);
            hasher.finish()
        }

        let test_pairs = [
            // Swapped device/inode
            (
                HardLinkKey {
                    device: 12345,
                    inode: 67890,
                },
                HardLinkKey {
                    device: 67890,
                    inode: 12345,
                },
            ),
            // Adjacent values
            (
                HardLinkKey {
                    device: 100,
                    inode: 100,
                },
                HardLinkKey {
                    device: 100,
                    inode: 101,
                },
            ),
            // High bits difference
            (
                HardLinkKey {
                    device: 1 << 63,
                    inode: 0,
                },
                HardLinkKey {
                    device: 0,
                    inode: 1 << 63,
                },
            ),
        ];

        for (key1, key2) in test_pairs {
            assert_ne!(key1, key2, "keys should not be equal");
            // Hashes might collide but it's acceptable; we just verify equality works
            if hash_key(&key1) == hash_key(&key2) {
                // Even with hash collision, equality should distinguish them
                assert_ne!(
                    key1, key2,
                    "equal hash but unequal keys should be distinguished"
                );
            }
        }
    }

    /// Test that Copy trait works correctly for HardLinkKey.
    #[test]
    fn hard_link_key_is_copy() {
        let key = HardLinkKey {
            device: 42,
            inode: 100,
        };
        let copy1 = key;
        let copy2 = key;

        // All should be equal
        assert_eq!(key, copy1);
        assert_eq!(copy1, copy2);
        assert_eq!(key.device, 42);
        assert_eq!(key.inode, 100);
    }
}

/// Tests for preservation behavior with the -H flag.
#[cfg(all(test, unix))]
mod preservation_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Test that first file in a hardlink group is copied, subsequent are linked.
    #[test]
    fn first_file_copied_subsequent_linked() {
        let temp = tempfile::tempdir().unwrap();
        let files: Vec<_> = (0..5)
            .map(|i| temp.path().join(format!("file{i}.txt")))
            .collect();

        // Create first file and hardlinks
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
                // First occurrence: no existing target
                assert!(
                    existing.is_none(),
                    "first file should not have existing target"
                );
            } else {
                // Subsequent: should have existing target
                assert!(existing.is_some(), "file {idx} should have existing target");
            }

            tracker.record(&metadata, &dest);
        }
    }

    /// Test that different content files with nlink=1 remain separate.
    #[test]
    fn standalone_files_remain_separate() {
        let temp = tempfile::tempdir().unwrap();
        let file1 = temp.path().join("file1.txt");
        let file2 = temp.path().join("file2.txt");

        std::fs::write(&file1, "content1").unwrap();
        std::fs::write(&file2, "content2").unwrap();

        let meta1 = std::fs::metadata(&file1).unwrap();
        let meta2 = std::fs::metadata(&file2).unwrap();

        // Both should have nlink = 1
        assert_eq!(meta1.nlink(), 1);
        assert_eq!(meta2.nlink(), 1);

        // Different inodes
        assert_ne!(meta1.ino(), meta2.ino());

        let mut tracker = HardLinkTracker::new();

        // Record both (should be no-op for nlink=1 files)
        tracker.record(&meta1, Path::new("/dest/file1.txt"));
        tracker.record(&meta2, Path::new("/dest/file2.txt"));

        // Neither should find existing target
        assert!(tracker.existing_target(&meta1).is_none());
        assert!(tracker.existing_target(&meta2).is_none());
    }

    /// Test behavior when hardlink group has mixed nlink values during sync.
    ///
    /// This can happen when some files in a group are deleted during transfer.
    #[test]
    fn handles_varying_nlink_values() {
        let temp = tempfile::tempdir().unwrap();
        let file1 = temp.path().join("file1.txt");
        let file2 = temp.path().join("file2.txt");
        let file3 = temp.path().join("file3.txt");

        std::fs::write(&file1, "content").unwrap();
        std::fs::hard_link(&file1, &file2).unwrap();
        std::fs::hard_link(&file1, &file3).unwrap();

        // All files now have nlink = 3
        let mut tracker = HardLinkTracker::new();

        // Record the first file
        let meta1 = std::fs::metadata(&file1).unwrap();
        tracker.record(&meta1, Path::new("/dest/file1.txt"));

        // Delete one link to change nlink
        std::fs::remove_file(&file3).unwrap();

        // Remaining files now have nlink = 2
        let meta2 = std::fs::metadata(&file2).unwrap();
        assert_eq!(meta2.nlink(), 2);

        // Should still find the existing target (same device/inode)
        let existing = tracker.existing_target(&meta2);
        assert!(
            existing.is_some(),
            "should find target even with changed nlink"
        );
    }
}

/// Tests for cross-device hardlink handling edge cases.
#[cfg(all(test, unix))]
mod cross_device_tests {
    use super::*;

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
}

/// Tests for large-scale scenarios.
#[cfg(all(test, unix))]
mod scale_tests {
    use super::*;

    /// Test tracking many hardlink groups.
    #[test]
    fn many_hardlink_groups() {
        let temp = tempfile::tempdir().unwrap();
        let num_groups = 50;
        let mut tracker = HardLinkTracker::new();

        for group in 0..num_groups {
            let file1 = temp.path().join(format!("group{group}_a.txt"));
            let file2 = temp.path().join(format!("group{group}_b.txt"));

            std::fs::write(&file1, format!("group{group} content")).unwrap();
            std::fs::hard_link(&file1, &file2).unwrap();

            let meta1 = std::fs::metadata(&file1).unwrap();
            let dest = PathBuf::from(format!("/dest/group{group}_a.txt"));
            tracker.record(&meta1, &dest);

            // Second file should find the destination
            let meta2 = std::fs::metadata(&file2).unwrap();
            let existing = tracker.existing_target(&meta2);
            assert_eq!(existing, Some(dest.clone()));
        }
    }

    /// Test many links to the same file.
    #[test]
    fn many_links_to_same_file() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.txt");
        std::fs::write(&original, "content").unwrap();

        let num_links = 100;
        let links: Vec<_> = (0..num_links)
            .map(|i| {
                let link = temp.path().join(format!("link{i}.txt"));
                std::fs::hard_link(&original, &link).unwrap();
                link
            })
            .collect();

        let mut tracker = HardLinkTracker::new();
        let original_meta = std::fs::metadata(&original).unwrap();
        let dest = PathBuf::from("/dest/original.txt");
        tracker.record(&original_meta, &dest);

        // All links should resolve to the same destination
        for (i, link) in links.iter().enumerate() {
            let meta = std::fs::metadata(link).unwrap();
            let existing = tracker.existing_target(&meta);
            assert_eq!(
                existing,
                Some(dest.clone()),
                "link {i} should resolve to original destination"
            );
        }
    }
}
