//! Hardlink tracking and deduplication for rsync protocol.
//!
//! During file list building and reception, hardlinks are identified by their
//! (device, inode) pairs. This module provides a table structure to track
//! hardlinks and assign unique indices for wire protocol transmission.
//!
//! Uses [`FxHashMap`] for fast lookups with integer-based keys.
//!
//! # Upstream Reference
//!
//! - `hlink.c:match_hlinkinfo()` - Hardlink matching logic
//! - `hlink.c:init_hard_links()` - Hardlink table initialization
//! - Protocol 30+ uses indices into a hardlink list

use rustc_hash::FxHashMap;

/// Device and inode pair identifying a unique file.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct DevIno {
    /// Device number.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
}

impl DevIno {
    /// Creates a new device/inode pair.
    #[must_use]
    pub const fn new(dev: u64, ino: u64) -> Self {
        Self { dev, ino }
    }
}

/// Entry in the hardlink table.
#[derive(Debug, Clone)]
pub struct HardlinkEntry {
    /// Index of the first file in the hardlink group.
    pub first_ndx: u32,
    /// Number of files in this hardlink group.
    pub link_count: u32,
}

impl HardlinkEntry {
    /// Creates a new hardlink entry.
    #[must_use]
    pub const fn new(first_ndx: u32) -> Self {
        Self {
            first_ndx,
            link_count: 1,
        }
    }
}

/// Result of looking up a hardlink in the table.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HardlinkLookup {
    /// This is the first occurrence of this file - assign a new index.
    First(u32),
    /// This is a subsequent occurrence - link to the first index.
    LinkTo(u32),
}

/// Table for tracking hardlinks by (dev, ino) pairs.
///
/// Used during file list building to deduplicate hardlinked files and assign
/// consistent indices for wire protocol transmission.
///
/// # Example
///
/// ```
/// use protocol::flist::{HardlinkTable, DevIno};
///
/// let mut table = HardlinkTable::new();
///
/// // First occurrence of a file
/// let result1 = table.find_or_insert(DevIno::new(0, 12345), 0);
/// assert!(matches!(result1, protocol::flist::HardlinkLookup::First(0)));
///
/// // Second occurrence (hardlink) - links back to first
/// let result2 = table.find_or_insert(DevIno::new(0, 12345), 1);
/// assert!(matches!(result2, protocol::flist::HardlinkLookup::LinkTo(0)));
/// ```
#[derive(Debug, Default)]
pub struct HardlinkTable {
    /// Map from (dev, ino) to hardlink entry.
    entries: FxHashMap<DevIno, HardlinkEntry>,
}

impl HardlinkTable {
    /// Creates a new empty hardlink table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a hardlink table with preallocated capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
        }
    }

    /// Looks up or inserts a hardlink entry.
    ///
    /// If this (dev, ino) pair is already in the table, returns `LinkTo` with
    /// the index of the first occurrence. Otherwise, inserts a new entry and
    /// returns `First` with the given file index.
    ///
    /// # Arguments
    ///
    /// * `dev_ino` - Device and inode pair identifying the file
    /// * `file_ndx` - Index of this file in the file list
    ///
    /// # Returns
    ///
    /// - `HardlinkLookup::First(ndx)` if this is the first occurrence
    /// - `HardlinkLookup::LinkTo(ndx)` if this links to a previous occurrence
    pub fn find_or_insert(&mut self, dev_ino: DevIno, file_ndx: u32) -> HardlinkLookup {
        match self.entries.get_mut(&dev_ino) {
            Some(entry) => {
                entry.link_count += 1;
                HardlinkLookup::LinkTo(entry.first_ndx)
            }
            None => {
                self.entries.insert(dev_ino, HardlinkEntry::new(file_ndx));
                HardlinkLookup::First(file_ndx)
            }
        }
    }

    /// Looks up a hardlink entry without modifying the table.
    ///
    /// Returns the entry if found, or `None` if the (dev, ino) pair is not in the table.
    #[must_use]
    pub fn get(&self, dev_ino: &DevIno) -> Option<&HardlinkEntry> {
        self.entries.get(dev_ino)
    }

    /// Returns the number of unique hardlink groups in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears all entries from the table.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_ino_new() {
        let di = DevIno::new(1, 2);
        assert_eq!(di.dev, 1);
        assert_eq!(di.ino, 2);
    }

    #[test]
    fn dev_ino_eq() {
        let a = DevIno::new(1, 2);
        let b = DevIno::new(1, 2);
        let c = DevIno::new(1, 3);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn dev_ino_hash() {
        use rustc_hash::FxHashSet;
        let mut set = FxHashSet::default();
        set.insert(DevIno::new(1, 2));
        assert!(set.contains(&DevIno::new(1, 2)));
        assert!(!set.contains(&DevIno::new(1, 3)));
    }

    #[test]
    fn hardlink_entry_new() {
        let entry = HardlinkEntry::new(42);
        assert_eq!(entry.first_ndx, 42);
        assert_eq!(entry.link_count, 1);
    }

    #[test]
    fn hardlink_table_new() {
        let table = HardlinkTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn hardlink_table_first_occurrence() {
        let mut table = HardlinkTable::new();
        let result = table.find_or_insert(DevIno::new(0, 12345), 0);
        assert_eq!(result, HardlinkLookup::First(0));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn hardlink_table_second_occurrence() {
        let mut table = HardlinkTable::new();
        table.find_or_insert(DevIno::new(0, 12345), 0);
        let result = table.find_or_insert(DevIno::new(0, 12345), 5);
        assert_eq!(result, HardlinkLookup::LinkTo(0));
        assert_eq!(table.len(), 1); // Still only one entry
    }

    #[test]
    fn hardlink_table_different_files() {
        let mut table = HardlinkTable::new();
        let r1 = table.find_or_insert(DevIno::new(0, 100), 0);
        let r2 = table.find_or_insert(DevIno::new(0, 200), 1);
        assert_eq!(r1, HardlinkLookup::First(0));
        assert_eq!(r2, HardlinkLookup::First(1));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn hardlink_table_link_count() {
        let mut table = HardlinkTable::new();
        let di = DevIno::new(0, 12345);
        table.find_or_insert(di, 0);
        table.find_or_insert(di, 5);
        table.find_or_insert(di, 10);

        let entry = table.get(&di).unwrap();
        assert_eq!(entry.link_count, 3);
        assert_eq!(entry.first_ndx, 0);
    }

    #[test]
    fn hardlink_table_get_nonexistent() {
        let table = HardlinkTable::new();
        assert!(table.get(&DevIno::new(0, 12345)).is_none());
    }

    #[test]
    fn hardlink_table_clear() {
        let mut table = HardlinkTable::new();
        table.find_or_insert(DevIno::new(0, 100), 0);
        table.find_or_insert(DevIno::new(0, 200), 1);
        assert_eq!(table.len(), 2);

        table.clear();
        assert!(table.is_empty());
    }

    #[test]
    fn hardlink_table_with_capacity() {
        let table = HardlinkTable::with_capacity(100);
        assert!(table.is_empty());
    }

    #[test]
    fn hardlink_table_different_devices() {
        let mut table = HardlinkTable::new();
        // Same inode on different devices - should be separate entries
        let r1 = table.find_or_insert(DevIno::new(1, 100), 0);
        let r2 = table.find_or_insert(DevIno::new(2, 100), 1);
        assert_eq!(r1, HardlinkLookup::First(0));
        assert_eq!(r2, HardlinkLookup::First(1));
        assert_eq!(table.len(), 2);
    }
}
