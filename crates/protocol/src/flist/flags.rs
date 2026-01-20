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

/// Flag indicating same rdev as previous entry (protocols 20-27).
///
/// In protocols before 28, this bit indicates the device number matches
/// the previous entry. Shares bit position with XMIT_EXTENDED_FLAGS.
/// Upstream: `XMIT_SAME_RDEV_pre28 (1<<2)` for protocols 20-27
pub const XMIT_SAME_RDEV_PRE28: u8 = 1 << 2;

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

/// Extended flag: same rdev major as previous (bit 8, devices only).
///
/// Upstream: `XMIT_SAME_RDEV_MAJOR (1<<8)` for protocol 28+ devices
pub const XMIT_SAME_RDEV_MAJOR: u8 = 1 << 0;

/// Extended flag: directory has no content to transfer (bit 8, directories only).
///
/// Used to mark implied directories or directories without content.
/// Shares bit position with XMIT_SAME_RDEV_MAJOR (directories vs devices).
/// Upstream: `XMIT_NO_CONTENT_DIR (1<<8)` for protocol 30+ directories
pub const XMIT_NO_CONTENT_DIR: u8 = 1 << 0;

/// Extended flag: entry has hardlink information (bit 9).
///
/// Upstream: `XMIT_HLINKED (1<<9)` for protocol 28+ non-directories
pub const XMIT_HLINKED: u8 = 1 << 1;

/// Extended flag: same device number as previous (bit 10, protocols 28-29).
///
/// Used for hardlink dev/ino encoding in protocols before 30.
/// Shares bit position with XMIT_USER_NAME_FOLLOWS (proto 28-29 vs 30+).
/// Upstream: `XMIT_SAME_DEV_pre30 (1<<10)` for protocols 28-29
pub const XMIT_SAME_DEV_PRE30: u8 = 1 << 2;

/// Extended flag: user name follows (bit 10, protocol 30+).
///
/// Upstream: `XMIT_USER_NAME_FOLLOWS (1<<10)`
pub const XMIT_USER_NAME_FOLLOWS: u8 = 1 << 2;

/// Extended flag: rdev minor fits in 8 bits (bit 11, protocols 28-29).
///
/// When set, minor device number is encoded as a single byte.
/// When clear, minor is encoded as a 4-byte int.
/// Shares bit position with XMIT_GROUP_NAME_FOLLOWS (proto 28-29 vs 30+).
/// Upstream: `XMIT_RDEV_MINOR_8_pre30 (1<<11)` for protocols 28-29
pub const XMIT_RDEV_MINOR_8_PRE30: u8 = 1 << 3;

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

/// Extended flag: same atime as previous entry (bit 14).
///
/// Used when `--atimes` is enabled. Restricted by command-line option.
/// Upstream: `XMIT_SAME_ATIME (1<<14)`
pub const XMIT_SAME_ATIME: u8 = 1 << 6;

/// Extended flag: unused (bit 15).
///
/// Reserved for future use.
/// Upstream: `XMIT_UNUSED_15 (1<<15)`
pub const XMIT_UNUSED_15: u8 = 1 << 7;

// Third byte of extended flags (bits 16-23 in varint mode)
//
// These flags are only available with VARINT_FLIST_FLAGS encoding.

/// Extended flag: reserved for fileflags (bit 16).
///
/// Upstream: `XMIT_RESERVED_16 (1<<16)`
pub const XMIT_RESERVED_16: u8 = 1 << 0;

/// Extended flag: creation time equals mtime (bit 17).
///
/// Used when `--crtimes` is enabled. If set, crtime equals mtime and is not
/// transmitted separately. Restricted by command-line option.
/// Upstream: `XMIT_CRTIME_EQ_MTIME (1<<17)`
pub const XMIT_CRTIME_EQ_MTIME: u8 = 1 << 1;

// Legacy alias for backward compatibility
#[allow(dead_code)]
pub const XMIT_SAME_HIGH_RDEV: u8 = XMIT_SAME_RDEV_MAJOR;

/// Parsed file entry flags from the wire format.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct FileFlags {
    /// First byte of flags (bits 0-7).
    pub primary: u8,
    /// Second byte of flags (bits 8-15, protocol 28+).
    pub extended: u8,
    /// Third byte of flags (bits 16-23, varint mode only).
    pub extended16: u8,
}

impl FileFlags {
    /// Creates flags from the raw bytes.
    #[must_use]
    pub const fn new(primary: u8, extended: u8) -> Self {
        Self {
            primary,
            extended,
            extended16: 0,
        }
    }

    /// Creates flags from three raw bytes (for varint mode).
    #[must_use]
    pub const fn new_with_extended16(primary: u8, extended: u8, extended16: u8) -> Self {
        Self {
            primary,
            extended,
            extended16,
        }
    }

    /// Creates flags from a u32 value (as decoded from varint).
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        Self {
            primary: value as u8,
            extended: (value >> 8) as u8,
            extended16: (value >> 16) as u8,
        }
    }

    /// Converts flags to a u32 value (for varint encoding).
    #[must_use]
    pub const fn to_u32(&self) -> u32 {
        (self.primary as u32) | ((self.extended as u32) << 8) | ((self.extended16 as u32) << 16)
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

    /// Returns true if this is the first entry in a hardlink group.
    #[inline]
    #[must_use]
    pub const fn hlink_first(&self) -> bool {
        self.extended & XMIT_HLINK_FIRST != 0
    }

    /// Returns true if a user name follows the UID.
    #[inline]
    #[must_use]
    pub const fn user_name_follows(&self) -> bool {
        self.extended & XMIT_USER_NAME_FOLLOWS != 0
    }

    /// Returns true if a group name follows the GID.
    #[inline]
    #[must_use]
    pub const fn group_name_follows(&self) -> bool {
        self.extended & XMIT_GROUP_NAME_FOLLOWS != 0
    }

    /// Returns true if this directory has no content to transfer.
    ///
    /// Only valid for directories. Shares bit position with `same_rdev_major()`.
    #[inline]
    #[must_use]
    pub const fn no_content_dir(&self) -> bool {
        self.extended & XMIT_NO_CONTENT_DIR != 0
    }

    /// Returns true if the entry shares atime with the previous entry.
    #[inline]
    #[must_use]
    pub const fn same_atime(&self) -> bool {
        self.extended & XMIT_SAME_ATIME != 0
    }

    /// Returns true if same device number as previous (protocols 28-29).
    #[inline]
    #[must_use]
    pub const fn same_dev_pre30(&self) -> bool {
        self.extended & XMIT_SAME_DEV_PRE30 != 0
    }

    /// Returns true if rdev minor fits in 8 bits (protocols 28-29).
    #[inline]
    #[must_use]
    pub const fn rdev_minor_8_pre30(&self) -> bool {
        self.extended & XMIT_RDEV_MINOR_8_PRE30 != 0
    }

    /// Returns true if creation time equals mtime (bits 16+, varint mode).
    #[inline]
    #[must_use]
    pub const fn crtime_eq_mtime(&self) -> bool {
        self.extended16 & XMIT_CRTIME_EQ_MTIME != 0
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
        assert_eq!(flags.extended16, 0);
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
        assert_eq!(XMIT_SAME_RDEV_PRE28, 0b0000_0100); // Same bit as EXTENDED_FLAGS for proto < 28
        assert_eq!(XMIT_SAME_UID, 0b0000_1000);
        assert_eq!(XMIT_SAME_GID, 0b0001_0000);
        assert_eq!(XMIT_SAME_NAME, 0b0010_0000);
        assert_eq!(XMIT_LONG_NAME, 0b0100_0000);
        assert_eq!(XMIT_SAME_TIME, 0b1000_0000);
    }

    #[test]
    fn xmit_same_rdev_pre28_same_as_extended_flags() {
        // These share the same bit position but are used in different protocol versions
        assert_eq!(XMIT_SAME_RDEV_PRE28, XMIT_EXTENDED_FLAGS);
    }

    #[test]
    fn extended_constants_have_expected_values() {
        assert_eq!(XMIT_SAME_RDEV_MAJOR, 0b0000_0001);
        assert_eq!(XMIT_NO_CONTENT_DIR, 0b0000_0001); // Same bit, different context
        assert_eq!(XMIT_HLINKED, 0b0000_0010);
        assert_eq!(XMIT_SAME_DEV_PRE30, 0b0000_0100); // Protocols 28-29
        assert_eq!(XMIT_USER_NAME_FOLLOWS, 0b0000_0100); // Protocol 30+
        assert_eq!(XMIT_RDEV_MINOR_8_PRE30, 0b0000_1000); // Protocols 28-29
        assert_eq!(XMIT_GROUP_NAME_FOLLOWS, 0b0000_1000); // Protocol 30+
        assert_eq!(XMIT_HLINK_FIRST, 0b0001_0000);
        assert_eq!(XMIT_MOD_NSEC, 0b0010_0000);
        assert_eq!(XMIT_SAME_ATIME, 0b0100_0000);
        assert_eq!(XMIT_UNUSED_15, 0b1000_0000);
    }

    #[test]
    fn extended16_constants_have_expected_values() {
        assert_eq!(XMIT_RESERVED_16, 0b0000_0001);
        assert_eq!(XMIT_CRTIME_EQ_MTIME, 0b0000_0010);
    }

    #[test]
    fn xmit_same_high_rdev_alias() {
        assert_eq!(XMIT_SAME_HIGH_RDEV, XMIT_SAME_RDEV_MAJOR);
    }

    #[test]
    fn xmit_io_error_endlist_alias() {
        assert_eq!(XMIT_IO_ERROR_ENDLIST, XMIT_HLINK_FIRST);
    }

    #[test]
    fn flags_hlink_first() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST);
        assert!(flags.hlink_first());
        assert!(!flags.hlinked());
    }

    #[test]
    fn flags_user_name_follows() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_USER_NAME_FOLLOWS);
        assert!(flags.user_name_follows());
        assert!(!flags.group_name_follows());
    }

    #[test]
    fn flags_group_name_follows() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_GROUP_NAME_FOLLOWS);
        assert!(flags.group_name_follows());
        assert!(!flags.user_name_follows());
    }

    #[test]
    fn flags_combined_extended() {
        let flags = FileFlags::new(
            XMIT_EXTENDED_FLAGS,
            XMIT_HLINKED | XMIT_HLINK_FIRST | XMIT_USER_NAME_FOLLOWS | XMIT_GROUP_NAME_FOLLOWS,
        );
        assert!(flags.hlinked());
        assert!(flags.hlink_first());
        assert!(flags.user_name_follows());
        assert!(flags.group_name_follows());
    }

    #[test]
    fn flags_no_content_dir() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_NO_CONTENT_DIR);
        assert!(flags.no_content_dir());
    }

    #[test]
    fn flags_same_atime() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_SAME_ATIME);
        assert!(flags.same_atime());
    }

    #[test]
    fn flags_same_dev_pre30() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_SAME_DEV_PRE30);
        assert!(flags.same_dev_pre30());
    }

    #[test]
    fn flags_rdev_minor_8_pre30() {
        let flags = FileFlags::new(XMIT_EXTENDED_FLAGS, XMIT_RDEV_MINOR_8_PRE30);
        assert!(flags.rdev_minor_8_pre30());
    }

    #[test]
    fn flags_crtime_eq_mtime() {
        let flags = FileFlags::new_with_extended16(0, 0, XMIT_CRTIME_EQ_MTIME);
        assert!(flags.crtime_eq_mtime());
    }

    #[test]
    fn flags_from_u32() {
        // Test with all three bytes
        let value: u32 = 0x020103; // extended16=0x02, extended=0x01, primary=0x03
        let flags = FileFlags::from_u32(value);
        assert_eq!(flags.primary, 0x03);
        assert_eq!(flags.extended, 0x01);
        assert_eq!(flags.extended16, 0x02);
    }

    #[test]
    fn flags_to_u32() {
        let flags = FileFlags::new_with_extended16(0x03, 0x01, 0x02);
        let value = flags.to_u32();
        assert_eq!(value, 0x020103);
    }

    #[test]
    fn flags_from_to_u32_round_trip() {
        let original: u32 = 0x1F3C7A;
        let flags = FileFlags::from_u32(original);
        let round_tripped = flags.to_u32();
        assert_eq!(original, round_tripped);
    }
}
