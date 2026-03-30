//! Hard link tracking for local copy operations.
//!
//! Provides two tracking strategies:
//!
//! - [`HardLinkTracker`] - source-side tracking by (device, inode) for local
//!   copies where the source filesystem exposes inode metadata.
//! - [`HardlinkApplyTracker`] - receiver-side tracking by `hardlink_idx` (gnum)
//!   for protocol 30+ transfers where hardlink groups are identified by wire
//!   index rather than filesystem metadata.
//!
//! Uses [`FxHashMap`] for fast lookups with integer keys.

use std::fs;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

/// Tracks completed hardlink leaders by protocol group index during file apply.
///
/// When the receiver commits a leader file to its final destination, it records
/// the `hardlink_idx` (gnum) and destination path. Subsequent followers with the
/// same gnum can then be created as hard links to the leader via `std::fs::hard_link`
/// instead of receiving a separate copy of the data.
///
/// Deferred followers whose leader has not yet been committed are collected and
/// resolved once the leader arrives. This handles out-of-order completion in
/// pipelined transfers.
///
/// # Upstream Reference
///
/// - `hlink.c:finish_hard_link()` - walks deferred follower list after leader transfer
/// - `hlink.c:hard_link_check()` - defers followers when leader is in-progress
pub struct HardlinkApplyTracker {
    /// Map from hardlink group index (gnum) to the leader's committed destination path.
    leaders: FxHashMap<u32, PathBuf>,
    /// Followers waiting for their leader to be committed.
    /// Key: leader gnum, Value: list of follower destination paths.
    deferred: FxHashMap<u32, Vec<PathBuf>>,
}

/// Result of attempting to apply a hardlink for a follower entry.
#[derive(Debug, PartialEq, Eq)]
pub enum HardlinkApplyResult {
    /// The leader was found and a hard link was created at the follower path.
    Linked,
    /// The leader has not been committed yet; the follower is deferred.
    Deferred,
}

impl HardlinkApplyTracker {
    /// Creates a new tracker with no recorded leaders.
    #[must_use]
    pub fn new() -> Self {
        Self {
            leaders: FxHashMap::default(),
            deferred: FxHashMap::default(),
        }
    }

    /// Records a leader file's committed destination path.
    ///
    /// Call this after the leader file has been fully written and renamed to its
    /// final destination. Any previously deferred followers for this gnum are
    /// returned so the caller can create hard links for them.
    ///
    /// # upstream: hlink.c:finish_hard_link() - creates links for deferred followers
    pub fn record_leader(&mut self, gnum: u32, dest: PathBuf) -> Vec<PathBuf> {
        self.leaders.insert(gnum, dest);
        self.deferred.remove(&gnum).unwrap_or_default()
    }

    /// Attempts to create a hard link for a follower entry.
    ///
    /// If the leader's destination is already known, creates the hard link and
    /// returns `Linked`. Otherwise, defers the follower and returns `Deferred`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the hard link syscall fails (e.g., cross-device,
    /// permission denied, destination already exists).
    ///
    /// # upstream: hlink.c:hard_link_check() - defers or links depending on leader state
    pub fn apply_follower(
        &mut self,
        gnum: u32,
        follower_dest: &Path,
    ) -> std::io::Result<HardlinkApplyResult> {
        if let Some(leader_path) = self.leaders.get(&gnum) {
            // Ensure parent directory exists for the follower.
            if let Some(parent) = follower_dest.parent() {
                fs::create_dir_all(parent)?;
            }
            // Remove existing file at follower path to avoid AlreadyExists.
            if follower_dest.symlink_metadata().is_ok() {
                fs::remove_file(follower_dest)?;
            }
            fs::hard_link(leader_path, follower_dest)?;
            Ok(HardlinkApplyResult::Linked)
        } else {
            self.deferred
                .entry(gnum)
                .or_default()
                .push(follower_dest.to_path_buf());
            Ok(HardlinkApplyResult::Deferred)
        }
    }

    /// Returns the leader destination path for a given group index, if known.
    #[must_use]
    pub fn leader_path(&self, gnum: u32) -> Option<&Path> {
        self.leaders.get(&gnum).map(PathBuf::as_path)
    }

    /// Returns the number of deferred followers across all groups.
    #[must_use]
    pub fn deferred_count(&self) -> usize {
        self.deferred.values().map(Vec::len).sum()
    }

    /// Returns the number of recorded leader groups.
    #[must_use]
    pub fn leader_count(&self) -> usize {
        self.leaders.len()
    }

    /// Resolves all remaining deferred followers by creating hard links.
    ///
    /// Returns the number of hard links successfully created and a list of
    /// errors for any that failed.
    ///
    /// # upstream: hlink.c:finish_hard_link() - final pass for remaining deferred entries
    pub fn resolve_deferred(&mut self) -> (usize, Vec<(PathBuf, std::io::Error)>) {
        let mut linked = 0;
        let mut errors = Vec::new();

        let deferred = std::mem::take(&mut self.deferred);
        for (gnum, followers) in deferred {
            let leader_path = match self.leaders.get(&gnum) {
                Some(p) => p.clone(),
                None => {
                    for follower in followers {
                        errors.push((
                            follower,
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!("hardlink leader for group {gnum} never committed"),
                            ),
                        ));
                    }
                    continue;
                }
            };

            for follower in followers {
                if let Some(parent) = follower.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        errors.push((follower, e));
                        continue;
                    }
                }
                if follower.symlink_metadata().is_ok() {
                    if let Err(e) = fs::remove_file(&follower) {
                        errors.push((follower, e));
                        continue;
                    }
                }
                match fs::hard_link(&leader_path, &follower) {
                    Ok(()) => linked += 1,
                    Err(e) => errors.push((follower, e)),
                }
            }
        }

        (linked, errors)
    }
}

impl Default for HardlinkApplyTracker {
    fn default() -> Self {
        Self::new()
    }
}

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
        let temp = test_support::create_tempdir();
        let file = temp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();
        let metadata = std::fs::metadata(&file).unwrap();

        let tracker = HardLinkTracker::new();
        assert!(tracker.existing_target(&metadata).is_none());
    }

    #[test]
    fn record_does_not_panic() {
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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

/// Tests for the protocol-aware `HardlinkApplyTracker`.
#[cfg(test)]
mod apply_tracker_tests {
    use super::*;

    #[test]
    fn new_tracker_is_empty() {
        let tracker = HardlinkApplyTracker::new();
        assert_eq!(tracker.leader_count(), 0);
        assert_eq!(tracker.deferred_count(), 0);
    }

    #[test]
    fn default_creates_empty_tracker() {
        let tracker = HardlinkApplyTracker::default();
        assert_eq!(tracker.leader_count(), 0);
    }

    #[test]
    fn record_leader_returns_empty_when_no_deferred() {
        let mut tracker = HardlinkApplyTracker::new();
        let deferred = tracker.record_leader(42, PathBuf::from("/dest/leader.txt"));
        assert!(deferred.is_empty());
        assert_eq!(tracker.leader_count(), 1);
    }

    #[test]
    fn leader_path_returns_recorded_path() {
        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(7, PathBuf::from("/dest/file.txt"));
        assert_eq!(
            tracker.leader_path(7),
            Some(Path::new("/dest/file.txt"))
        );
        assert!(tracker.leader_path(99).is_none());
    }

    #[test]
    fn follower_linked_when_leader_exists() {
        let temp = test_support::create_tempdir();
        let leader_path = temp.path().join("leader.txt");
        let follower_path = temp.path().join("follower.txt");
        std::fs::write(&leader_path, "shared content").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(10, leader_path.clone());

        let result = tracker.apply_follower(10, &follower_path).unwrap();
        assert_eq!(result, HardlinkApplyResult::Linked);
        assert_eq!(
            std::fs::read_to_string(&follower_path).unwrap(),
            "shared content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn follower_shares_inode_with_leader() {
        use std::os::unix::fs::MetadataExt;

        let temp = test_support::create_tempdir();
        let leader_path = temp.path().join("leader.txt");
        let follower_path = temp.path().join("follower.txt");
        std::fs::write(&leader_path, "content").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(5, leader_path.clone());
        tracker.apply_follower(5, &follower_path).unwrap();

        let leader_meta = std::fs::metadata(&leader_path).unwrap();
        let follower_meta = std::fs::metadata(&follower_path).unwrap();
        assert_eq!(leader_meta.ino(), follower_meta.ino());
        assert_eq!(leader_meta.dev(), follower_meta.dev());
        assert!(leader_meta.nlink() >= 2);
    }

    #[test]
    fn follower_deferred_when_leader_missing() {
        let mut tracker = HardlinkApplyTracker::new();
        let result = tracker
            .apply_follower(42, Path::new("/dest/follower.txt"))
            .unwrap();
        assert_eq!(result, HardlinkApplyResult::Deferred);
        assert_eq!(tracker.deferred_count(), 1);
    }

    #[test]
    fn record_leader_returns_deferred_followers() {
        let mut tracker = HardlinkApplyTracker::new();

        // Defer two followers
        tracker
            .apply_follower(10, Path::new("/dest/f1.txt"))
            .unwrap();
        tracker
            .apply_follower(10, Path::new("/dest/f2.txt"))
            .unwrap();
        assert_eq!(tracker.deferred_count(), 2);

        // Record the leader - should return deferred followers
        let deferred = tracker.record_leader(10, PathBuf::from("/dest/leader.txt"));
        assert_eq!(deferred.len(), 2);
        assert_eq!(deferred[0], PathBuf::from("/dest/f1.txt"));
        assert_eq!(deferred[1], PathBuf::from("/dest/f2.txt"));
        assert_eq!(tracker.deferred_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn deferred_followers_resolved_after_leader_committed() {
        use std::os::unix::fs::MetadataExt;

        let temp = test_support::create_tempdir();
        let leader_path = temp.path().join("leader.txt");
        let follower1 = temp.path().join("follower1.txt");
        let follower2 = temp.path().join("follower2.txt");

        let mut tracker = HardlinkApplyTracker::new();

        // Followers arrive before leader
        tracker.apply_follower(20, &follower1).unwrap();
        tracker.apply_follower(20, &follower2).unwrap();
        assert_eq!(tracker.deferred_count(), 2);

        // Leader committed to disk
        std::fs::write(&leader_path, "deferred content").unwrap();
        let deferred = tracker.record_leader(20, leader_path.clone());

        // Caller creates links for deferred followers
        for follower_dest in &deferred {
            std::fs::hard_link(&leader_path, follower_dest).unwrap();
        }

        // Verify all share the same inode
        let leader_ino = std::fs::metadata(&leader_path).unwrap().ino();
        assert_eq!(std::fs::metadata(&follower1).unwrap().ino(), leader_ino);
        assert_eq!(std::fs::metadata(&follower2).unwrap().ino(), leader_ino);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_across_directories() {
        use std::os::unix::fs::MetadataExt;

        let temp = test_support::create_tempdir();
        let dir_a = temp.path().join("dir_a");
        let dir_b = temp.path().join("dir_b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let leader_path = dir_a.join("file.txt");
        let follower_path = dir_b.join("file.txt");
        std::fs::write(&leader_path, "cross-dir content").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(30, leader_path.clone());

        let result = tracker.apply_follower(30, &follower_path).unwrap();
        assert_eq!(result, HardlinkApplyResult::Linked);

        let leader_meta = std::fs::metadata(&leader_path).unwrap();
        let follower_meta = std::fs::metadata(&follower_path).unwrap();
        assert_eq!(leader_meta.ino(), follower_meta.ino());
        assert_eq!(
            std::fs::read_to_string(&follower_path).unwrap(),
            "cross-dir content"
        );
    }

    #[test]
    fn multiple_independent_groups() {
        let temp = test_support::create_tempdir();
        let leader1 = temp.path().join("group1_leader.txt");
        let leader2 = temp.path().join("group2_leader.txt");
        let follower1 = temp.path().join("group1_follower.txt");
        let follower2 = temp.path().join("group2_follower.txt");

        std::fs::write(&leader1, "group1").unwrap();
        std::fs::write(&leader2, "group2").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(100, leader1.clone());
        tracker.record_leader(200, leader2.clone());

        tracker.apply_follower(100, &follower1).unwrap();
        tracker.apply_follower(200, &follower2).unwrap();

        assert_eq!(std::fs::read_to_string(&follower1).unwrap(), "group1");
        assert_eq!(std::fs::read_to_string(&follower2).unwrap(), "group2");
    }

    #[test]
    fn follower_replaces_existing_file() {
        let temp = test_support::create_tempdir();
        let leader = temp.path().join("leader.txt");
        let follower = temp.path().join("follower.txt");

        std::fs::write(&leader, "correct content").unwrap();
        std::fs::write(&follower, "old content").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(50, leader.clone());

        let result = tracker.apply_follower(50, &follower).unwrap();
        assert_eq!(result, HardlinkApplyResult::Linked);
        assert_eq!(
            std::fs::read_to_string(&follower).unwrap(),
            "correct content"
        );
    }

    #[test]
    fn follower_creates_parent_directories() {
        let temp = test_support::create_tempdir();
        let leader = temp.path().join("leader.txt");
        let follower = temp.path().join("deep/nested/dir/follower.txt");

        std::fs::write(&leader, "content").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(60, leader.clone());

        let result = tracker.apply_follower(60, &follower).unwrap();
        assert_eq!(result, HardlinkApplyResult::Linked);
        assert!(follower.exists());
        assert_eq!(std::fs::read_to_string(&follower).unwrap(), "content");
    }

    #[test]
    fn resolve_deferred_creates_links() {
        let temp = test_support::create_tempdir();
        let leader = temp.path().join("leader.txt");
        let follower1 = temp.path().join("f1.txt");
        let follower2 = temp.path().join("f2.txt");

        let mut tracker = HardlinkApplyTracker::new();

        // Defer followers
        tracker.apply_follower(70, &follower1).unwrap();
        tracker.apply_follower(70, &follower2).unwrap();

        // Write and record leader
        std::fs::write(&leader, "resolved content").unwrap();
        // Re-insert deferred into tracker for resolve_deferred to handle
        let deferred_list = tracker.record_leader(70, leader.clone());

        // Manually re-defer them for the resolve path
        let mut tracker2 = HardlinkApplyTracker::new();
        tracker2.record_leader(70, leader.clone());
        for path in &deferred_list {
            // Simulate re-deferral by directly adding
            tracker2.deferred.entry(70).or_default().push(path.clone());
        }

        let (linked, errors) = tracker2.resolve_deferred();
        assert_eq!(linked, 2);
        assert!(errors.is_empty());
        assert!(follower1.exists());
        assert!(follower2.exists());
    }

    #[test]
    fn resolve_deferred_reports_missing_leader() {
        let mut tracker = HardlinkApplyTracker::new();
        // Manually insert a deferred follower for a non-existent leader
        tracker
            .deferred
            .entry(999)
            .or_default()
            .push(PathBuf::from("/nonexistent/follower.txt"));

        let (linked, errors) = tracker.resolve_deferred();
        assert_eq!(linked, 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, PathBuf::from("/nonexistent/follower.txt"));
    }

    #[test]
    fn many_followers_in_one_group() {
        let temp = test_support::create_tempdir();
        let leader = temp.path().join("leader.txt");
        std::fs::write(&leader, "shared").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(80, leader.clone());

        for i in 0..50 {
            let follower = temp.path().join(format!("follower_{i}.txt"));
            let result = tracker.apply_follower(80, &follower).unwrap();
            assert_eq!(result, HardlinkApplyResult::Linked);
            assert_eq!(std::fs::read_to_string(&follower).unwrap(), "shared");
        }
    }

    #[cfg(unix)]
    #[test]
    fn all_followers_share_single_inode() {
        use std::os::unix::fs::MetadataExt;

        let temp = test_support::create_tempdir();
        let leader = temp.path().join("leader.txt");
        std::fs::write(&leader, "inode-check").unwrap();

        let mut tracker = HardlinkApplyTracker::new();
        tracker.record_leader(90, leader.clone());

        let leader_ino = std::fs::metadata(&leader).unwrap().ino();

        for i in 0..10 {
            let follower = temp.path().join(format!("f{i}.txt"));
            tracker.apply_follower(90, &follower).unwrap();
            assert_eq!(std::fs::metadata(&follower).unwrap().ino(), leader_ino);
        }

        // nlink should be 11 (1 leader + 10 followers)
        assert_eq!(std::fs::metadata(&leader).unwrap().nlink(), 11);
    }
}
