//! Hardlink detection and resolution for rsync transfers.
//!
//! This module implements the upstream rsync hardlink algorithm from `hlink.c`.
//! When `--hard-links` (`-H`) is enabled, rsync detects files that are hardlinked
//! together (sharing the same device/inode) and recreates those links on the
//! destination instead of transferring the file data multiple times.
//!
//! # Algorithm Overview
//!
//! 1. **Detection**: During file list building, collect `(dev, ino)` pairs for
//!    all files with `nlink > 1`.
//! 2. **Grouping**: Group files by their `(dev, ino)` to identify hardlink sets.
//! 3. **Resolution**: For each group:
//!    - The first file is the **source** (transferred normally)
//!    - Subsequent files are **links** (created as hardlinks to the first)
//! 4. **Protocol encoding**:
//!    - Protocol 30+: Uses incremental indices into the hardlink list
//!    - Protocol 28-29: Uses `(dev, ino)` pairs directly
//!
//! # Cross-device Safety
//!
//! Files on different devices (different `dev` values) are **never** linked,
//! even if they have the same inode number. This is required because hardlinks
//! cannot cross filesystem boundaries.
//!
//! # Example
//!
//! ```
//! use engine::hardlink::{HardlinkTracker, HardlinkKey, HardlinkAction};
//!
//! let mut tracker = HardlinkTracker::new();
//!
//! // Register files with same device/inode (hardlinks)
//! let key = HardlinkKey::new(0xFD00, 12345);
//! tracker.register(key, 0); // First occurrence
//! tracker.register(key, 5); // Second occurrence
//! tracker.register(key, 10); // Third occurrence
//!
//! // Resolve what action to take for each file
//! assert_eq!(tracker.resolve(0), HardlinkAction::Transfer); // Source file
//! assert_eq!(tracker.resolve(5), HardlinkAction::LinkTo(0)); // Link to first
//! assert_eq!(tracker.resolve(10), HardlinkAction::LinkTo(0)); // Link to first
//!
//! // Different device, same inode - not linked
//! let key2 = HardlinkKey::new(0xFD01, 12345);
//! tracker.register(key2, 15);
//! assert_eq!(tracker.resolve(15), HardlinkAction::Transfer); // Different file
//! ```
//!
//! # Upstream Reference
//!
//! - `hlink.c:init_hard_links()` - Hardlink table initialization
//! - `hlink.c:match_hard_links()` - Hardlink matching logic
//! - Protocol 30+: Uses `XMIT_HLINKED` and `XMIT_HLINK_FIRST` flags
//! - Protocol 28-29: Uses `XMIT_SAME_DEV_PRE30` flag

use rustc_hash::FxHashMap;
use std::collections::HashMap;

/// Device and inode pair identifying a unique file.
///
/// This is the key used to detect hardlinks. Files with the same `(dev, ino)`
/// pair are hardlinks to the same underlying inode.
///
/// # Cross-device Behavior
///
/// Files on different devices (different `dev` values) are always treated as
/// distinct files, even if they have the same inode number. This matches the
/// semantics of filesystem hardlinks, which cannot cross device boundaries.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct HardlinkKey {
    /// Device number (st_dev from stat).
    pub dev: u64,
    /// Inode number (st_ino from stat).
    pub ino: u64,
}

impl HardlinkKey {
    /// Creates a new hardlink key from device and inode numbers.
    #[must_use]
    pub const fn new(dev: u64, ino: u64) -> Self {
        Self { dev, ino }
    }
}

/// A group of files sharing the same inode (hardlinks).
///
/// Each group has a **source** (the first file registered) and zero or more
/// **links** (additional files that should be created as hardlinks to the source).
#[derive(Debug, Clone)]
pub struct HardlinkGroup {
    /// The (dev, ino) pair for this group.
    pub key: HardlinkKey,
    /// Index of the source file (first in the group).
    pub source_index: i32,
    /// Indices of files that should link to the source.
    pub link_indices: Vec<i32>,
}

impl HardlinkGroup {
    /// Creates a new hardlink group with a single source file.
    #[must_use]
    pub fn new(key: HardlinkKey, source_index: i32) -> Self {
        Self {
            key,
            source_index,
            link_indices: Vec::new(),
        }
    }

    /// Adds a link to this group.
    pub fn add_link(&mut self, index: i32) {
        self.link_indices.push(index);
    }

    /// Returns the total number of files in this group (source + links).
    #[must_use]
    pub fn total_count(&self) -> usize {
        1 + self.link_indices.len()
    }
}

/// Action to take for a file during transfer.
///
/// This determines whether a file should be transferred normally or created
/// as a hardlink to an earlier file.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HardlinkAction {
    /// Transfer this file normally (it's the source of a hardlink group or not hardlinked).
    Transfer,
    /// Create this file as a hardlink to the file at the given index.
    LinkTo(i32),
    /// Skip this file (useful for filtering).
    Skip,
}

/// Tracker for hardlink detection and resolution.
///
/// This maintains the mapping from `(dev, ino)` pairs to file indices, allowing
/// efficient detection of hardlinked files and resolution of transfer actions.
///
/// # Thread Safety
///
/// This structure is not thread-safe. Use separate trackers per thread or
/// synchronize access externally.
///
/// # Performance
///
/// Uses [`FxHashMap`] for O(1) average-case lookups. Registration and resolution
/// are both O(1) operations.
#[derive(Debug)]
pub struct HardlinkTracker {
    /// Map from (dev, ino) to the first file index and subsequent link indices.
    groups: FxHashMap<HardlinkKey, HardlinkGroup>,
    /// Map from file index to hardlink action.
    actions: HashMap<i32, HardlinkAction>,
}

impl HardlinkTracker {
    /// Creates a new empty hardlink tracker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            groups: FxHashMap::default(),
            actions: HashMap::new(),
        }
    }

    /// Creates a hardlink tracker with preallocated capacity.
    ///
    /// Use this when you know approximately how many hardlink groups to expect.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            groups: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
            actions: HashMap::with_capacity(capacity * 2), // Estimate 2 files per group
        }
    }

    /// Registers a file with its device/inode pair.
    ///
    /// If this is the first file with this `(dev, ino)`, it becomes the source.
    /// Subsequent files with the same key become links to the first.
    ///
    /// # Arguments
    ///
    /// * `key` - Device and inode pair from `stat()`
    /// * `file_index` - Index of this file in the file list
    ///
    /// # Returns
    ///
    /// `true` if this is the first file with this key (source), `false` if it's a link.
    pub fn register(&mut self, key: HardlinkKey, file_index: i32) -> bool {
        match self.groups.get_mut(&key) {
            Some(group) => {
                // Subsequent occurrence - add as link
                group.add_link(file_index);
                self.actions.insert(file_index, HardlinkAction::LinkTo(group.source_index));
                false
            }
            None => {
                // First occurrence - create new group
                let group = HardlinkGroup::new(key, file_index);
                self.groups.insert(key, group);
                self.actions.insert(file_index, HardlinkAction::Transfer);
                true
            }
        }
    }

    /// Resolves the action to take for a file.
    ///
    /// # Arguments
    ///
    /// * `file_index` - Index of the file in the file list
    ///
    /// # Returns
    ///
    /// - `HardlinkAction::Transfer` if the file should be transferred normally
    /// - `HardlinkAction::LinkTo(source)` if it should be hardlinked to `source`
    /// - `HardlinkAction::Skip` if the file was never registered (shouldn't happen)
    #[must_use]
    pub fn resolve(&self, file_index: i32) -> HardlinkAction {
        self.actions.get(&file_index).copied().unwrap_or(HardlinkAction::Skip)
    }

    /// Checks if a file is the source of a hardlink group.
    ///
    /// # Arguments
    ///
    /// * `file_index` - Index of the file in the file list
    ///
    /// # Returns
    ///
    /// `true` if this file is the first in its hardlink group, `false` otherwise.
    #[must_use]
    pub fn is_hardlink_source(&self, file_index: i32) -> bool {
        matches!(self.resolve(file_index), HardlinkAction::Transfer)
            && self.groups.values().any(|g| g.source_index == file_index && !g.link_indices.is_empty())
    }

    /// Gets the hardlink target index for a file.
    ///
    /// # Arguments
    ///
    /// * `file_index` - Index of the file in the file list
    ///
    /// # Returns
    ///
    /// - `Some(source_index)` if this file should be linked to `source_index`
    /// - `None` if this file should be transferred normally or was never registered
    #[must_use]
    pub fn get_hardlink_target(&self, file_index: i32) -> Option<i32> {
        match self.resolve(file_index) {
            HardlinkAction::LinkTo(target) => Some(target),
            _ => None,
        }
    }

    /// Returns an iterator over all hardlink groups.
    ///
    /// Only groups with at least one link (total count >= 2) are yielded.
    pub fn groups(&self) -> impl Iterator<Item = &HardlinkGroup> {
        self.groups.values().filter(|g| !g.link_indices.is_empty())
    }

    /// Returns the number of hardlink groups (with at least 2 files each).
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.groups.values().filter(|g| !g.link_indices.is_empty()).count()
    }

    /// Returns the total number of registered files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.actions.len()
    }

    /// Clears all registered hardlinks.
    pub fn clear(&mut self) {
        self.groups.clear();
        self.actions.clear();
    }
}

impl Default for HardlinkTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolver for hardlink actions during transfer.
///
/// This is a lightweight wrapper around hardlink resolution logic that can
/// be used independently of the tracker.
#[derive(Debug)]
pub struct HardlinkResolver;

impl HardlinkResolver {
    /// Resolves the action for a file given a tracker.
    ///
    /// This is a convenience method equivalent to calling `tracker.resolve()`.
    #[must_use]
    pub fn resolve(tracker: &HardlinkTracker, file_index: i32) -> HardlinkAction {
        tracker.resolve(file_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardlink_key_new() {
        let key = HardlinkKey::new(100, 12345);
        assert_eq!(key.dev, 100);
        assert_eq!(key.ino, 12345);
    }

    #[test]
    fn hardlink_key_eq() {
        let k1 = HardlinkKey::new(100, 12345);
        let k2 = HardlinkKey::new(100, 12345);
        let k3 = HardlinkKey::new(100, 12346);
        let k4 = HardlinkKey::new(101, 12345);

        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k1, k4);
    }

    #[test]
    fn hardlink_group_new() {
        let key = HardlinkKey::new(1, 2);
        let group = HardlinkGroup::new(key, 42);
        assert_eq!(group.source_index, 42);
        assert!(group.link_indices.is_empty());
        assert_eq!(group.total_count(), 1);
    }

    #[test]
    fn hardlink_group_add_link() {
        let key = HardlinkKey::new(1, 2);
        let mut group = HardlinkGroup::new(key, 0);
        group.add_link(5);
        group.add_link(10);
        assert_eq!(group.link_indices, vec![5, 10]);
        assert_eq!(group.total_count(), 3);
    }

    #[test]
    fn tracker_new() {
        let tracker = HardlinkTracker::new();
        assert_eq!(tracker.file_count(), 0);
        assert_eq!(tracker.group_count(), 0);
    }

    #[test]
    fn tracker_single_hardlink_group() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(0xFD00, 12345);

        // First file - should be source
        assert!(tracker.register(key, 0));
        assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);

        // Second file - should link to first
        assert!(!tracker.register(key, 5));
        assert_eq!(tracker.resolve(5), HardlinkAction::LinkTo(0));

        // Third file - should also link to first
        assert!(!tracker.register(key, 10));
        assert_eq!(tracker.resolve(10), HardlinkAction::LinkTo(0));

        assert_eq!(tracker.file_count(), 3);
        assert_eq!(tracker.group_count(), 1);
    }

    #[test]
    fn tracker_multiple_groups() {
        let mut tracker = HardlinkTracker::new();
        let key1 = HardlinkKey::new(1, 100);
        let key2 = HardlinkKey::new(1, 200);

        // Group 1
        tracker.register(key1, 0);
        tracker.register(key1, 1);

        // Group 2
        tracker.register(key2, 2);
        tracker.register(key2, 3);
        tracker.register(key2, 4);

        assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
        assert_eq!(tracker.resolve(1), HardlinkAction::LinkTo(0));
        assert_eq!(tracker.resolve(2), HardlinkAction::Transfer);
        assert_eq!(tracker.resolve(3), HardlinkAction::LinkTo(2));
        assert_eq!(tracker.resolve(4), HardlinkAction::LinkTo(2));

        assert_eq!(tracker.group_count(), 2);
    }

    #[test]
    fn tracker_cross_device_not_linked() {
        let mut tracker = HardlinkTracker::new();

        // Same inode, different devices - should be separate
        let key1 = HardlinkKey::new(0, 12345);
        let key2 = HardlinkKey::new(1, 12345);

        tracker.register(key1, 0);
        tracker.register(key2, 1);

        // Both should be sources, not linked
        assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
        assert_eq!(tracker.resolve(1), HardlinkAction::Transfer);
        assert_eq!(tracker.group_count(), 0); // No groups with links
    }

    #[test]
    fn tracker_files_with_nlink_1() {
        let mut tracker = HardlinkTracker::new();

        // Single file, no hardlinks
        let key = HardlinkKey::new(1, 100);
        tracker.register(key, 0);

        assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
        assert_eq!(tracker.group_count(), 0); // No groups with links
    }

    #[test]
    fn tracker_is_hardlink_source() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(1, 100);

        tracker.register(key, 0);
        tracker.register(key, 1);

        assert!(tracker.is_hardlink_source(0));
        assert!(!tracker.is_hardlink_source(1));
    }

    #[test]
    fn tracker_get_hardlink_target() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(1, 100);

        tracker.register(key, 0);
        tracker.register(key, 1);

        assert_eq!(tracker.get_hardlink_target(0), None);
        assert_eq!(tracker.get_hardlink_target(1), Some(0));
    }

    #[test]
    fn tracker_groups_iterator() {
        let mut tracker = HardlinkTracker::new();
        let key1 = HardlinkKey::new(1, 100);
        let key2 = HardlinkKey::new(1, 200);
        let key3 = HardlinkKey::new(1, 300);

        // Group 1: 2 files
        tracker.register(key1, 0);
        tracker.register(key1, 1);

        // Group 2: 3 files
        tracker.register(key2, 2);
        tracker.register(key2, 3);
        tracker.register(key2, 4);

        // Group 3: 1 file (should not be in iterator)
        tracker.register(key3, 5);

        let groups: Vec<_> = tracker.groups().collect();
        assert_eq!(groups.len(), 2);

        // Verify group contents
        let mut total_files = 0;
        for group in groups {
            total_files += group.total_count();
        }
        assert_eq!(total_files, 5); // 2 + 3
    }

    #[test]
    fn tracker_clear() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(1, 100);

        tracker.register(key, 0);
        tracker.register(key, 1);
        assert_eq!(tracker.file_count(), 2);

        tracker.clear();
        assert_eq!(tracker.file_count(), 0);
        assert_eq!(tracker.group_count(), 0);
    }

    #[test]
    fn tracker_with_capacity() {
        let tracker = HardlinkTracker::with_capacity(100);
        assert_eq!(tracker.file_count(), 0);
    }

    #[test]
    fn resolver_resolve() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(1, 100);

        tracker.register(key, 0);
        tracker.register(key, 1);

        assert_eq!(HardlinkResolver::resolve(&tracker, 0), HardlinkAction::Transfer);
        assert_eq!(HardlinkResolver::resolve(&tracker, 1), HardlinkAction::LinkTo(0));
    }

    #[test]
    fn large_hardlink_group() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(1, 12345);

        // Create a group with 1000 hardlinks
        for i in 0..1000 {
            tracker.register(key, i);
        }

        // First should be source
        assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
        assert!(tracker.is_hardlink_source(0));

        // All others should link to first
        for i in 1..1000 {
            assert_eq!(tracker.resolve(i), HardlinkAction::LinkTo(0));
            assert_eq!(tracker.get_hardlink_target(i), Some(0));
        }

        assert_eq!(tracker.group_count(), 1);
        assert_eq!(tracker.file_count(), 1000);
    }

    #[test]
    fn extreme_device_inode_values() {
        let mut tracker = HardlinkTracker::new();

        let cases = [
            (0, 0),
            (u64::MAX, u64::MAX),
            (0, u64::MAX),
            (u64::MAX, 0),
        ];

        for (i, &(dev, ino)) in cases.iter().enumerate() {
            let key = HardlinkKey::new(dev, ino);
            tracker.register(key, i as i32);
        }

        // All should be separate (no links)
        for i in 0..cases.len() {
            assert_eq!(tracker.resolve(i as i32), HardlinkAction::Transfer);
        }
    }

    #[test]
    fn negative_file_indices() {
        let mut tracker = HardlinkTracker::new();
        let key = HardlinkKey::new(1, 100);

        // Negative indices should work (incremental file list uses them)
        tracker.register(key, -1);
        tracker.register(key, -2);

        assert_eq!(tracker.resolve(-1), HardlinkAction::Transfer);
        assert_eq!(tracker.resolve(-2), HardlinkAction::LinkTo(-1));
    }
}
