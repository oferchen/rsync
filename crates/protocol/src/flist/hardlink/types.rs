//! Core types for hardlink tracking.
//!
//! Defines the data structures used to identify and group hardlinked files
//! by their (device, inode) pairs.

/// Device and inode pair identifying a unique file.
///
/// upstream: hlink.c struct idev - dev/ino pair for hardlink tracking
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

/// Entry in the hardlink table tracking the first occurrence and link count.
///
/// upstream: hlink.c struct hlink - tracks first file and nlink count
#[derive(Debug, Clone)]
pub struct HardlinkEntry {
    /// Index of the first file in the hardlink group.
    pub first_ndx: u32,
    /// Number of files in this hardlink group.
    pub link_count: u32,
}

impl HardlinkEntry {
    /// Creates a new hardlink entry with a link count of 1.
    #[must_use]
    pub const fn new(first_ndx: u32) -> Self {
        Self {
            first_ndx,
            link_count: 1,
        }
    }
}

/// Result of looking up a hardlink in the table.
///
/// Determines whether a file is the first occurrence (leader) or a subsequent
/// link (follower) in a hardlink group.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HardlinkLookup {
    /// This is the first occurrence of this file - assign a new index.
    First(u32),
    /// This is a subsequent occurrence - link to the first index.
    LinkTo(u32),
}
