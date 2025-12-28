//! File entry flags for the rsync flist wire format.
//!
//! The flags byte indicates which fields are present in a file entry and how
//! they are encoded. These constants match upstream rsync's `rsync.h`.
//!
//! # Upstream Reference
//!
//! Flag bit positions from rsync.h (protocol 28+):
//! - Bits 0-7: Primary flags (first byte/varint)
//! - Bits 8-15: Extended flags (second byte, protocol 28+)
//! - Bits 16+: Additional extended flags (protocol 30+)

#![allow(dead_code)] // Many constants reserved for future protocol features

// Primary flags (bits 0-7) - matches upstream rsync.h

/// Flag indicating this is the top-level directory in the transfer.
///
/// Used to mark directories that should not be deleted during `--delete`.
/// Upstream: `XMIT_TOP_DIR (1<<0)`
pub const XMIT_TOP_DIR: u8 = 1 << 0;

/// Flag indicating the entry has the same mode as the previous entry.
///
/// Upstream: `XMIT_SAME_MODE (1<<1)`
pub const XMIT_SAME_MODE: u8 = 1 << 1;

/// Flag indicating that extended flags follow the first byte.
///
/// When set in protocol 28+, additional flag bits are encoded.
/// With VARINT_FLIST_FLAGS, flags are encoded as a single varint.
/// Upstream: `XMIT_EXTENDED_FLAGS (1<<2)` for protocol 28+
pub const XMIT_EXTENDED_FLAGS: u8 = 1 << 2;

/// Flag indicating the entry has the same UID as the previous entry.
///
/// Upstream: `XMIT_SAME_UID (1<<3)`
pub const XMIT_SAME_UID: u8 = 1 << 3;

/// Flag indicating the entry has the same GID as the previous entry.
///
/// Upstream: `XMIT_SAME_GID (1<<4)`
pub const XMIT_SAME_GID: u8 = 1 << 4;

/// Flag indicating the entry has the same file name as the previous entry.
///
/// When set, a same_len byte follows to indicate how many prefix bytes match.
/// Upstream: `XMIT_SAME_NAME (1<<5)`
pub const XMIT_SAME_NAME: u8 = 1 << 5;

/// Flag indicating the name length uses a varint instead of 8-bit.
///
/// Used for paths longer than 255 bytes.
/// Upstream: `XMIT_LONG_NAME (1<<6)`
pub const XMIT_LONG_NAME: u8 = 1 << 6;

/// Flag indicating the entry has the same modification time as the previous entry.
///
/// Upstream: `XMIT_SAME_TIME (1<<7)`
pub const XMIT_SAME_TIME: u8 = 1 << 7;

// Extended flags (bits 8-15 in varint, or second byte in protocol 28-29)
//
// These correspond to upstream bits (1<<8) through (1<<15).
// When stored as a separate byte, these are bits 0-7 of that byte.
// When encoded as a varint with VARINT_FLIST_FLAGS, they occupy bits 8-15.

/// Extended flag: same rdev major as previous (bit 8).
///
/// Upstream: `XMIT_SAME_RDEV_MAJOR (1<<8)` for protocol 28+ devices
pub const XMIT_SAME_RDEV_MAJOR: u8 = 1 << 0;

/// Extended flag: entry has hardlink information (bit 9).
///
/// Upstream: `XMIT_HLINKED (1<<9)` for protocol 28+ non-directories
pub const XMIT_HLINKED: u8 = 1 << 1;

/// Extended flag: user name follows (bit 10, protocol 30+).
///
/// Upstream: `XMIT_USER_NAME_FOLLOWS (1<<10)`
pub const XMIT_USER_NAME_FOLLOWS: u8 = 1 << 2;

/// Extended flag: group name follows (bit 11, protocol 30+).
///
/// Upstream: `XMIT_GROUP_NAME_FOLLOWS (1<<11)`
pub const XMIT_GROUP_NAME_FOLLOWS: u8 = 1 << 3;

/// Extended flag: hardlink first / I/O error end list (bit 12).
///
/// Upstream: `XMIT_HLINK_FIRST (1<<12)` for protocol 30+ non-dirs
/// Upstream: `XMIT_IO_ERROR_ENDLIST (1<<12)` for protocol 31+ end marker
pub const XMIT_HLINK_FIRST: u8 = 1 << 4;
pub const XMIT_IO_ERROR_ENDLIST: u8 = 1 << 4;

/// Extended flag: mtime has nanoseconds (bit 13, protocol 31+).
///
/// Upstream: `XMIT_MOD_NSEC (1<<13)`
pub const XMIT_MOD_NSEC: u8 = 1 << 5;

/// Extended flag: same ACL as previous entry (bit 14).
///
/// Upstream: `XMIT_SAME_ACL (1<<14)` (restricted feature)
pub const XMIT_SAME_ACL: u8 = 1 << 6;

/// Extended flag: same xattr as previous entry (bit 15).
///
/// Upstream: `XMIT_SAME_XATTR (1<<15)` (restricted feature)
pub const XMIT_SAME_XATTR: u8 = 1 << 7;

// Legacy alias for backward compatibility
#[allow(dead_code)]
pub const XMIT_SAME_HIGH_RDEV: u8 = XMIT_SAME_RDEV_MAJOR;

/// Parsed file entry flags from the wire format.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct FileFlags {
    /// First byte of flags.
    pub primary: u8,
    /// Second byte of flags (protocol 28+).
    pub extended: u8,
}

impl FileFlags {
    /// Creates flags from the raw bytes.
    #[must_use]
    pub const fn new(primary: u8, extended: u8) -> Self {
        Self { primary, extended }
    }

    /// Returns true if extended flags are present.
    #[inline]
    #[must_use]
    pub const fn has_extended(&self) -> bool {
        self.primary & XMIT_EXTENDED_FLAGS != 0
    }

    /// Returns true if the entry shares the UID with the previous entry.
    #[inline]
    #[must_use]
    pub const fn same_uid(&self) -> bool {
        self.primary & XMIT_SAME_UID != 0
    }

    /// Returns true if the entry shares the GID with the previous entry.
    #[inline]
    #[must_use]
    pub const fn same_gid(&self) -> bool {
        self.primary & XMIT_SAME_GID != 0
    }

    /// Returns true if the entry shares part of its name with the previous entry.
    #[inline]
    #[must_use]
    pub const fn same_name(&self) -> bool {
        self.primary & XMIT_SAME_NAME != 0
    }

    /// Returns true if the name length is encoded as a 32-bit integer.
    #[inline]
    #[must_use]
    pub const fn long_name(&self) -> bool {
        self.primary & XMIT_LONG_NAME != 0
    }

    /// Returns true if the entry shares the mtime with the previous entry.
    #[inline]
    #[must_use]
    pub const fn same_time(&self) -> bool {
        self.primary & XMIT_SAME_TIME != 0
    }

    /// Returns true if the entry shares the mode with the previous entry.
    #[inline]
    #[must_use]
    pub const fn same_mode(&self) -> bool {
        self.primary & XMIT_SAME_MODE != 0
    }

    /// Returns true if this is a top-level directory.
    #[inline]
    #[must_use]
    pub const fn top_dir(&self) -> bool {
        self.primary & XMIT_TOP_DIR != 0
    }

    /// Returns true if the entry has hardlink information.
    #[inline]
    #[must_use]
    pub const fn hlinked(&self) -> bool {
        self.extended & XMIT_HLINKED != 0
    }

    /// Returns true if the entry shares rdev major with the previous entry (device).
    #[inline]
    #[must_use]
    pub const fn same_high_rdev(&self) -> bool {
        self.extended & XMIT_SAME_RDEV_MAJOR != 0
    }

    /// Returns true if the entry shares rdev major with previous.
    #[inline]
    #[must_use]
    pub const fn same_rdev_major(&self) -> bool {
        self.extended & XMIT_SAME_RDEV_MAJOR != 0
    }

    /// Returns true if mtime includes nanoseconds.
    #[inline]
    #[must_use]
    pub const fn mod_nsec(&self) -> bool {
        self.extended & XMIT_MOD_NSEC != 0
    }

    /// Returns true if this entry marks the end of the list or an I/O error.
    #[inline]
    #[must_use]
    pub const fn io_error_endlist(&self) -> bool {
        self.extended & XMIT_IO_ERROR_ENDLIST != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_default_is_zero() {
        let flags = FileFlags::default();
        assert_eq!(flags.primary, 0);
        assert_eq!(flags.extended, 0);
    }

    #[test]
    fn flags_same_name_detection() {
        let flags = FileFlags::new(XMIT_SAME_NAME, 0);
        assert!(flags.same_name());
        assert!(!flags.same_uid());
        assert!(!flags.has_extended());
    }

    #[test]
    fn flags_extended_detection() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_HLINKED);
        assert!(flags.has_extended());
        assert!(flags.hlinked());
    }

    #[test]
    fn flags_combined_primary() {
        let flags = FileFlags::new(XMIT_SAME_UID | XMIT_SAME_GID | XMIT_SAME_TIME, 0);
        assert!(flags.same_uid());
        assert!(flags.same_gid());
        assert!(flags.same_time());
        assert!(!flags.same_mode());
    }

    #[test]
    fn flags_long_name() {
        let flags = FileFlags::new(XMIT_LONG_NAME, 0);
        assert!(flags.long_name());
    }

    #[test]
    fn flags_same_mode() {
        let flags = FileFlags::new(XMIT_SAME_MODE, 0);
        assert!(flags.same_mode());
    }

    #[test]
    fn flags_top_dir() {
        let flags = FileFlags::new(XMIT_TOP_DIR, 0);
        assert!(flags.top_dir());
    }

    #[test]
    fn flags_same_high_rdev() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_SAME_RDEV_MAJOR);
        assert!(flags.same_high_rdev());
        assert!(flags.same_rdev_major());
    }

    #[test]
    fn flags_mod_nsec() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_MOD_NSEC);
        assert!(flags.mod_nsec());
    }

    #[test]
    fn flags_io_error_endlist() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST);
        assert!(flags.io_error_endlist());
    }

    #[test]
    fn file_flags_clone() {
        let flags = FileFlags::new(XMIT_SAME_NAME, XMIT_HLINKED);
        let cloned = flags;
        assert_eq!(flags, cloned);
    }

    #[test]
    fn file_flags_eq() {
        let a = FileFlags::new(XMIT_SAME_NAME, XMIT_HLINKED);
        let b = FileFlags::new(XMIT_SAME_NAME, XMIT_HLINKED);
        let c = FileFlags::new(XMIT_SAME_UID, 0);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn xmit_constants_have_expected_values() {
        assert_eq!(XMIT_TOP_DIR, 0b0000_0001);
        assert_eq!(XMIT_SAME_MODE, 0b0000_0010);
        assert_eq!(XMIT_EXTENDED_FLAGS, 0b0000_0100);
        assert_eq!(XMIT_SAME_UID, 0b0000_1000);
        assert_eq!(XMIT_SAME_GID, 0b0001_0000);
        assert_eq!(XMIT_SAME_NAME, 0b0010_0000);
        assert_eq!(XMIT_LONG_NAME, 0b0100_0000);
        assert_eq!(XMIT_SAME_TIME, 0b1000_0000);
    }

    #[test]
    fn extended_constants_have_expected_values() {
        assert_eq!(XMIT_SAME_RDEV_MAJOR, 0b0000_0001);
        assert_eq!(XMIT_HLINKED, 0b0000_0010);
        assert_eq!(XMIT_USER_NAME_FOLLOWS, 0b0000_0100);
        assert_eq!(XMIT_GROUP_NAME_FOLLOWS, 0b0000_1000);
        assert_eq!(XMIT_HLINK_FIRST, 0b0001_0000);
        assert_eq!(XMIT_MOD_NSEC, 0b0010_0000);
        assert_eq!(XMIT_SAME_ACL, 0b0100_0000);
        assert_eq!(XMIT_SAME_XATTR, 0b1000_0000);
    }

    #[test]
    fn xmit_same_high_rdev_alias() {
        assert_eq!(XMIT_SAME_HIGH_RDEV, XMIT_SAME_RDEV_MAJOR);
    }

    #[test]
    fn xmit_io_error_endlist_alias() {
        assert_eq!(XMIT_IO_ERROR_ENDLIST, XMIT_HLINK_FIRST);
    }
}
