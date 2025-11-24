//! File entry flags for the rsync flist wire format.
//!
//! The flags byte indicates which fields are present in a file entry and how
//! they are encoded. These constants match upstream rsync's `flist.c`.

#![allow(dead_code)] // Many constants reserved for future protocol features

/// Flag indicating that extended flags follow the first byte.
///
/// When set, an additional byte is read to get more flag bits.
/// Used in protocol versions 28+.
pub const XMIT_EXTENDED_FLAGS: u8 = 1 << 0;

/// Flag indicating the entry has the same UID as the previous entry.
pub const XMIT_SAME_UID: u8 = 1 << 1;

/// Flag indicating the entry has the same GID as the previous entry.
pub const XMIT_SAME_GID: u8 = 1 << 2;

/// Flag indicating the entry has the same file name as the previous entry.
///
/// When set, only a length byte follows to indicate how many bytes differ.
pub const XMIT_SAME_NAME: u8 = 1 << 3;

/// Flag indicating the name length uses a 32-bit integer instead of 8-bit.
///
/// Used for paths longer than 255 bytes.
pub const XMIT_LONG_NAME: u8 = 1 << 4;

/// Flag indicating the entry has the same modification time as the previous entry.
pub const XMIT_SAME_TIME: u8 = 1 << 5;

/// Flag indicating the entry has the same mode as the previous entry.
pub const XMIT_SAME_MODE: u8 = 1 << 6;

/// Flag indicating this is the top-level directory in the transfer.
///
/// Used to mark directories that should not be deleted during `--delete`.
pub const XMIT_TOP_DIR: u8 = 1 << 7;

// Extended flags (second byte, protocol 28+):

/// Extended flag indicating the entry has a high bit set in its mode.
///
/// Used for device files and other special modes.
pub const XMIT_SAME_HIGH_RDEV: u8 = 1 << 0;

/// Extended flag indicating the entry has a different rdev major number.
pub const XMIT_SAME_RDEV_MAJOR: u8 = 1 << 1;

/// Extended flag indicating the entry has hardlink information.
pub const XMIT_HLINKED: u8 = 1 << 2;

/// Extended flag indicating the entry uses hardlink first information.
pub const XMIT_HLINK_FIRST: u8 = 1 << 3;

/// Extended flag indicating I/O error or special handling needed.
pub const XMIT_IO_ERROR_ENDLIST: u8 = 1 << 4;

/// Extended flag indicating modification time uses 32-bit seconds only.
pub const XMIT_MOD_NSEC: u8 = 1 << 5;

/// Extended flag indicating same ACL as previous entry.
pub const XMIT_SAME_ACL: u8 = 1 << 6;

/// Extended flag indicating same xattr as previous entry.
pub const XMIT_SAME_XATTR: u8 = 1 << 7;

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
    #[must_use]
    pub const fn has_extended(&self) -> bool {
        self.primary & XMIT_EXTENDED_FLAGS != 0
    }

    /// Returns true if the entry shares the UID with the previous entry.
    #[must_use]
    pub const fn same_uid(&self) -> bool {
        self.primary & XMIT_SAME_UID != 0
    }

    /// Returns true if the entry shares the GID with the previous entry.
    #[must_use]
    pub const fn same_gid(&self) -> bool {
        self.primary & XMIT_SAME_GID != 0
    }

    /// Returns true if the entry shares part of its name with the previous entry.
    #[must_use]
    pub const fn same_name(&self) -> bool {
        self.primary & XMIT_SAME_NAME != 0
    }

    /// Returns true if the name length is encoded as a 32-bit integer.
    #[must_use]
    pub const fn long_name(&self) -> bool {
        self.primary & XMIT_LONG_NAME != 0
    }

    /// Returns true if the entry shares the mtime with the previous entry.
    #[must_use]
    pub const fn same_time(&self) -> bool {
        self.primary & XMIT_SAME_TIME != 0
    }

    /// Returns true if the entry shares the mode with the previous entry.
    #[must_use]
    pub const fn same_mode(&self) -> bool {
        self.primary & XMIT_SAME_MODE != 0
    }

    /// Returns true if this is a top-level directory.
    #[must_use]
    pub const fn top_dir(&self) -> bool {
        self.primary & XMIT_TOP_DIR != 0
    }

    /// Returns true if the entry has hardlink information.
    #[must_use]
    pub const fn hlinked(&self) -> bool {
        self.extended & XMIT_HLINKED != 0
    }

    /// Returns true if the entry has high rdev bits.
    #[must_use]
    pub const fn same_high_rdev(&self) -> bool {
        self.extended & XMIT_SAME_HIGH_RDEV != 0
    }

    /// Returns true if the entry shares rdev major with previous.
    #[must_use]
    pub const fn same_rdev_major(&self) -> bool {
        self.extended & XMIT_SAME_RDEV_MAJOR != 0
    }

    /// Returns true if mtime includes nanoseconds.
    #[must_use]
    pub const fn mod_nsec(&self) -> bool {
        self.extended & XMIT_MOD_NSEC != 0
    }

    /// Returns true if this entry marks the end of the list or an I/O error.
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
}
