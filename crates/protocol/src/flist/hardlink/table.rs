//! Hardlink lookup table for deduplication during file list exchange.
//!
//! Tracks hardlinks by (dev, ino) pairs and assigns consistent indices
//! for wire protocol transmission.
//!
//! # Upstream Reference
//!
//! - `hlink.c:match_hlinkinfo()` - Hardlink matching logic
//! - `hlink.c:init_hard_links()` - Hardlink table initialization
//! - Protocol 30+ uses indices into a hardlink list

use rustc_hash::FxHashMap;

use super::types::{DevIno, HardlinkEntry, HardlinkLookup};

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
