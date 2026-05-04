//! Transmission flag (xflags) calculation for file list entries.
//!
//! Computes compressed flags that indicate which fields differ from the
//! previous entry, enabling delta compression of the file list.
//!
//! # Upstream Reference
//!
//! See `flist.c:send_file_entry()` lines 475-550 for the xflags calculation.

use super::super::entry::FileEntry;
use super::super::flags::{
    XMIT_CRTIME_EQ_MTIME, XMIT_GROUP_NAME_FOLLOWS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_LONG_NAME,
    XMIT_MOD_NSEC, XMIT_NO_CONTENT_DIR, XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_ATIME,
    XMIT_SAME_DEV_PRE30, XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR,
    XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_TOP_DIR, XMIT_USER_NAME_FOLLOWS,
};
use super::FileListWriter;

impl FileListWriter {
    /// Calculates transmission flags (xflags) for an entry based on comparison with previous entry.
    ///
    /// The xflags are a compressed representation of which fields differ from the previous entry
    /// and control what data is transmitted on the wire. This enables delta compression of the
    /// file list by omitting unchanged fields.
    ///
    /// # Wire Format
    ///
    /// The xflags are divided into three bytes:
    /// - **Byte 0 (bits 0-7)**: Basic flags, always present
    /// - **Byte 1 (bits 8-15)**: Extended flags, present when `XMIT_EXTENDED_FLAGS` is set
    /// - **Byte 2 (bits 16-23)**: Extra flags, used in varint mode for creation time
    ///
    /// # Basic Flags (byte 0)
    ///
    /// | Flag | Bit | Meaning |
    /// |------|-----|---------|
    /// | `XMIT_TOP_DIR` | 0 | Entry is a command-line argument directory |
    /// | `XMIT_SAME_MODE` | 1 | Mode unchanged from previous entry |
    /// | `XMIT_SAME_RDEV_PRE28` | 2 | Same rdev (protocol < 28 only) |
    /// | `XMIT_SAME_UID` | 3 | UID unchanged from previous entry |
    /// | `XMIT_SAME_GID` | 4 | GID unchanged from previous entry |
    /// | `XMIT_SAME_NAME` | 5 | Name shares prefix with previous entry |
    /// | `XMIT_LONG_NAME` | 6 | Name suffix > 255 bytes |
    /// | `XMIT_SAME_TIME` | 7 | Mtime unchanged from previous entry |
    ///
    /// # Extended Flags (byte 1, when `XMIT_EXTENDED_FLAGS` set)
    ///
    /// | Flag | Bit | Meaning |
    /// |------|-----|---------|
    /// | `XMIT_SAME_RDEV_MAJOR` | 8 | Same rdev major (protocol 28+, devices/specials) |
    /// | `XMIT_NO_CONTENT_DIR` | 8 | Directory has no content (protocol 30+, directories) |
    /// | `XMIT_HLINKED` | 9 | Entry is a hardlink (protocol 28+) |
    /// | `XMIT_SAME_DEV_PRE30` | 10 | Same hardlink device (protocol 28-29) |
    /// | `XMIT_RDEV_MINOR_8_PRE30` | 10 | Rdev minor fits in byte (protocol 28-29) |
    /// | `XMIT_USER_NAME_FOLLOWS` | 11 | User name follows UID (protocol 30+) |
    /// | `XMIT_HLINK_FIRST` | 12 | First occurrence of hardlink (protocol 30+) |
    /// | `XMIT_IO_ERROR_ENDLIST` | 12 | End marker with I/O error (protocol 31+) |
    /// | `XMIT_GROUP_NAME_FOLLOWS` | 13 | Group name follows GID (protocol 30+) |
    /// | `XMIT_MOD_NSEC` | 14 | Mtime has nanoseconds (protocol 31+) |
    /// | `XMIT_SAME_ATIME` | 15 | Atime unchanged (when preserving atimes) |
    ///
    /// # Arguments
    ///
    /// * `entry` - The file entry to calculate flags for
    /// * `same_len` - Number of bytes shared with previous entry's name (prefix compression)
    /// * `suffix_len` - Length of the name suffix (portion not shared with previous)
    ///
    /// # Upstream Reference
    ///
    /// See `flist.c:send_file_entry()` lines 475-550 for the xflags calculation logic.
    pub(super) fn calculate_xflags(
        &self,
        entry: &FileEntry,
        same_len: usize,
        suffix_len: usize,
    ) -> u32 {
        let mut xflags = self.calculate_basic_flags(entry, same_len, suffix_len);
        xflags |= self.calculate_device_flags(entry);
        xflags |= self.calculate_hardlink_flags(entry);
        xflags |= self.calculate_owner_name_flags(entry, xflags);
        xflags |= self.calculate_time_flags(entry);
        xflags |= self.calculate_directory_flags(entry);
        xflags
    }

    /// Calculates basic transmission flags for mode, time, uid, gid, and name compression.
    ///
    /// These flags occupy byte 0 of the xflags field and are common across all protocol versions.
    #[inline]
    fn calculate_basic_flags(&self, entry: &FileEntry, same_len: usize, suffix_len: usize) -> u32 {
        let mut xflags: u32 = 0;

        if entry.is_dir() && entry.flags().top_dir() {
            xflags |= XMIT_TOP_DIR as u32;
        }

        if entry.mode() == self.state.prev_mode() {
            xflags |= XMIT_SAME_MODE as u32;
        }

        if entry.mtime() == self.state.prev_mtime() {
            xflags |= XMIT_SAME_TIME as u32;
        }

        // upstream: flist.c:463 - set XMIT_SAME_UID when !preserve_uid OR
        // (uid matches previous AND not the first entry). The *lastname guard
        // prevents false "same" on the first entry where prev_uid is zero.
        let entry_uid = entry.uid().unwrap_or(0);
        let not_first_entry = !self.state.prev_name().is_empty();
        if !self.preserve.uid || (entry_uid == self.state.prev_uid() && not_first_entry) {
            xflags |= XMIT_SAME_UID as u32;
        }

        // upstream: flist.c:473 - same pattern as UID
        let entry_gid = entry.gid().unwrap_or(0);
        if !self.preserve.gid || (entry_gid == self.state.prev_gid() && not_first_entry) {
            xflags |= XMIT_SAME_GID as u32;
        }

        if same_len > 0 {
            xflags |= XMIT_SAME_NAME as u32;
        }

        if suffix_len > 255 {
            xflags |= XMIT_LONG_NAME as u32;
        }

        xflags
    }

    /// Calculates device-related flags for block/character devices and special files.
    ///
    /// Handles XMIT_SAME_RDEV_MAJOR and XMIT_RDEV_MINOR_8_PRE30 flags.
    /// These flags occupy byte 1 (shifted by 8 bits).
    #[inline]
    fn calculate_device_flags(&self, entry: &FileEntry) -> u32 {
        let mut xflags: u32 = 0;

        let is_device = entry.is_device();
        let is_special = entry.is_special();

        // upstream: flist.c:send_file_entry() checks preserve_devices for
        // IS_DEVICE and preserve_specials for IS_SPECIAL separately
        let needs_rdev = (self.preserve.devices && is_device)
            || (self.preserve.specials && is_special && self.protocol.as_u8() < 31);

        if !needs_rdev {
            return xflags;
        }

        if is_special {
            // upstream: flist.c:450-460 - special files don't need a real rdev.
            // Set XMIT_SAME_RDEV_MAJOR unconditionally for efficiency, and
            // also XMIT_RDEV_MINOR_8_PRE30 for protocol 28-29.
            xflags |= (XMIT_SAME_RDEV_MAJOR as u32) << 8;
            if self.protocol.as_u8() >= 28 && self.protocol.as_u8() < 30 {
                xflags |= (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
            }
        } else {
            let major = entry.rdev_major().unwrap_or(0);
            if major == self.state.prev_rdev_major() {
                xflags |= (XMIT_SAME_RDEV_MAJOR as u32) << 8;
            }
            // Protocol 28-29: set XMIT_RDEV_MINOR_8_PRE30 when minor fits in a byte.
            if self.protocol.as_u8() >= 28 && self.protocol.as_u8() < 30 {
                let minor = entry.rdev_minor().unwrap_or(0);
                if minor <= 0xFF {
                    xflags |= (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
                }
            }
        }

        xflags
    }

    /// Calculates hardlink-related flags for protocol 28+.
    ///
    /// Handles XMIT_HLINKED, XMIT_HLINK_FIRST (protocol 30+) and
    /// XMIT_SAME_DEV_PRE30 (protocol 28-29).
    #[inline]
    fn calculate_hardlink_flags(&self, entry: &FileEntry) -> u32 {
        let mut xflags: u32 = 0;

        if !self.preserve.hard_links || entry.is_dir() {
            return xflags;
        }

        if self.protocol.as_u8() >= 30 {
            if let Some(idx) = entry.hardlink_idx() {
                xflags |= (XMIT_HLINKED as u32) << 8;
                if idx == u32::MAX {
                    xflags |= (XMIT_HLINK_FIRST as u32) << 8;
                }
            }
        } else if self.protocol.as_u8() >= 28 {
            if let Some(dev) = entry.hardlink_dev() {
                // upstream: flist.c:530 - XMIT_HLINKED set for ALL hardlink entries,
                // not just protocol 30+. Protocol 28-29 also sets this flag.
                xflags |= (XMIT_HLINKED as u32) << 8;
                if dev == self.state.prev_hardlink_dev() {
                    xflags |= (XMIT_SAME_DEV_PRE30 as u32) << 8;
                }
            }
        }

        xflags
    }

    /// Calculates user/group name flags for protocol 30+.
    ///
    /// Handles XMIT_USER_NAME_FOLLOWS and XMIT_GROUP_NAME_FOLLOWS.
    /// These require the corresponding SAME_UID/SAME_GID flags to NOT be set.
    #[inline]
    fn calculate_owner_name_flags(&self, entry: &FileEntry, current_flags: u32) -> u32 {
        let mut xflags: u32 = 0;

        if self.protocol.as_u8() < 30 {
            return xflags;
        }

        if self.preserve.uid
            && entry.user_name().is_some()
            && (current_flags & (XMIT_SAME_UID as u32)) == 0
        {
            xflags |= (XMIT_USER_NAME_FOLLOWS as u32) << 8;
        }

        if self.preserve.gid
            && entry.group_name().is_some()
            && (current_flags & (XMIT_SAME_GID as u32)) == 0
        {
            xflags |= (XMIT_GROUP_NAME_FOLLOWS as u32) << 8;
        }

        xflags
    }

    /// Calculates time-related flags including atime, crtime, and mtime nanoseconds.
    ///
    /// Handles XMIT_SAME_ATIME, XMIT_CRTIME_EQ_MTIME, and XMIT_MOD_NSEC.
    #[inline]
    fn calculate_time_flags(&self, entry: &FileEntry) -> u32 {
        let mut xflags: u32 = 0;

        if self.preserve.atimes && !entry.is_dir() && entry.atime() == self.state.prev_atime() {
            xflags |= (XMIT_SAME_ATIME as u32) << 8;
        }

        // XMIT_CRTIME_EQ_MTIME occupies bit 17, which is only transmitted in varint
        // flag encoding. In non-varint mode (protocol 28-29 two-byte flags), bits 16+
        // are not on the wire, so setting this flag would cause the writer to skip
        // crtime while the reader still expects it - leading to deserialization
        // misalignment.
        if self.use_varint_flags() && self.preserve.crtimes && entry.crtime() == entry.mtime() {
            xflags |= (XMIT_CRTIME_EQ_MTIME as u32) << 16;
        }

        if self.protocol.as_u8() >= 31 && entry.mtime_nsec() != 0 {
            xflags |= (XMIT_MOD_NSEC as u32) << 8;
        }

        xflags
    }

    /// Calculates directory-specific flags for protocol 30+.
    ///
    /// Handles XMIT_NO_CONTENT_DIR flag which indicates a directory
    /// whose contents should not be transferred.
    #[inline]
    fn calculate_directory_flags(&self, entry: &FileEntry) -> u32 {
        let mut xflags: u32 = 0;

        // XMIT_NO_CONTENT_DIR shares its bit position with XMIT_SAME_RDEV_MAJOR
        // (directories vs devices), so the flag is gated on entry.is_dir().
        if entry.is_dir() && self.protocol.as_u8() >= 30 && !entry.content_dir() {
            xflags |= (XMIT_NO_CONTENT_DIR as u32) << 8;
        }

        xflags
    }
}
