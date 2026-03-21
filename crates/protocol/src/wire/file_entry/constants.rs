//! XMIT flag constants for file entry wire format encoding.
//!
//! These constants define the wire format flags used in file entry
//! transmission, matching upstream rsync's `flist.c` definitions.

// Primary flags (bits 0-7)

/// Flag indicating this is the top-level directory in the transfer.
pub const XMIT_TOP_DIR: u8 = 1 << 0;

/// Flag indicating the entry has the same mode as the previous entry.
pub const XMIT_SAME_MODE: u8 = 1 << 1;

/// Flag indicating that extended flags follow the first byte (protocol 28+).
pub const XMIT_EXTENDED_FLAGS: u8 = 1 << 2;

/// Flag indicating same rdev as previous entry (protocols 20-27).
/// Shares bit position with `XMIT_EXTENDED_FLAGS`.
pub const XMIT_SAME_RDEV_PRE28: u8 = 1 << 2;

/// Flag indicating the entry has the same UID as the previous entry.
pub const XMIT_SAME_UID: u8 = 1 << 3;

/// Flag indicating the entry has the same GID as the previous entry.
pub const XMIT_SAME_GID: u8 = 1 << 4;

/// Flag indicating the entry shares part of its name with the previous entry.
pub const XMIT_SAME_NAME: u8 = 1 << 5;

/// Flag indicating the name length uses a varint instead of 8-bit.
pub const XMIT_LONG_NAME: u8 = 1 << 6;

/// Flag indicating the entry has the same modification time as the previous entry.
pub const XMIT_SAME_TIME: u8 = 1 << 7;

// Extended flags (bits 8-15, stored as byte 1 of extended flags)

/// Extended flag: same rdev major as previous (bit 8, devices only).
pub const XMIT_SAME_RDEV_MAJOR: u8 = 1 << 0;

/// Extended flag: directory has no content to transfer (bit 8, directories only).
pub const XMIT_NO_CONTENT_DIR: u8 = 1 << 0;

/// Extended flag: entry has hardlink information (bit 9).
pub const XMIT_HLINKED: u8 = 1 << 1;

/// Extended flag: same device number as previous (bit 10, protocols 28-29).
pub const XMIT_SAME_DEV_PRE30: u8 = 1 << 2;

/// Extended flag: user name follows (bit 10, protocol 30+).
pub const XMIT_USER_NAME_FOLLOWS: u8 = 1 << 2;

/// Extended flag: rdev minor fits in 8 bits (bit 11, protocols 28-29).
pub const XMIT_RDEV_MINOR_8_PRE30: u8 = 1 << 3;

/// Extended flag: group name follows (bit 11, protocol 30+).
pub const XMIT_GROUP_NAME_FOLLOWS: u8 = 1 << 3;

/// Extended flag: hardlink first / I/O error end list (bit 12).
pub const XMIT_HLINK_FIRST: u8 = 1 << 4;

/// Extended flag: I/O error end list marker (bit 12, protocol 31+).
pub const XMIT_IO_ERROR_ENDLIST: u8 = 1 << 4;

/// Extended flag: mtime has nanoseconds (bit 13, protocol 31+).
pub const XMIT_MOD_NSEC: u8 = 1 << 5;

/// Extended flag: same atime as previous entry (bit 14).
pub const XMIT_SAME_ATIME: u8 = 1 << 6;

// Third byte of extended flags (bits 16-23, varint mode only)

/// Extended flag: creation time equals mtime (bit 17).
pub const XMIT_CRTIME_EQ_MTIME: u8 = 1 << 1;
