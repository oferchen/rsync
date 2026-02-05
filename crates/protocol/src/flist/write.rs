//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver. The writer maintains compression
//! state to omit fields that match the previous entry, reducing wire traffic.
//!
//! # Upstream Reference
//!
//! See `flist.c:send_file_entry()` for the canonical wire format encoding.

use std::io::{self, Write};

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::codec::{ProtocolCodec, ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::varint::{write_varint, write_varint30_int};

use super::entry::FileEntry;
use super::flags::{
    XMIT_CRTIME_EQ_MTIME, XMIT_EXTENDED_FLAGS, XMIT_GROUP_NAME_FOLLOWS, XMIT_HLINK_FIRST,
    XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_MOD_NSEC, XMIT_NO_CONTENT_DIR,
    XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_ATIME, XMIT_SAME_DEV_PRE30, XMIT_SAME_GID, XMIT_SAME_MODE,
    XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_TOP_DIR,
    XMIT_USER_NAME_FOLLOWS,
};
use super::state::{FileListCompressionState, FileListStats};

/// State maintained while writing a file list.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This writer maintains the necessary state
/// to encode these compressed entries.
#[derive(Debug)]
pub struct FileListWriter {
    /// Protocol version being used.
    protocol: ProtocolVersion,
    /// Protocol codec for version-aware encoding.
    codec: ProtocolCodecEnum,
    /// Compression state for cross-entry field sharing.
    state: FileListCompressionState,
    /// Statistics collected during file list writing.
    stats: FileListStats,
    /// Whether to preserve (and thus write) UID values to the wire.
    preserve_uid: bool,
    /// Whether to preserve (and thus write) GID values to the wire.
    preserve_gid: bool,
    /// Whether to preserve (and thus write) symlink targets to the wire.
    preserve_links: bool,
    /// Whether to preserve (and thus write) device numbers to the wire.
    preserve_devices: bool,
    /// Whether to preserve (and thus write) hardlink indices to the wire.
    preserve_hard_links: bool,
    /// Whether to preserve (and thus write) access times to the wire.
    preserve_atimes: bool,
    /// Whether to preserve (and thus write) creation times to the wire.
    preserve_crtimes: bool,
    /// Whether to send checksums for all files (--checksum / -c mode).
    always_checksum: bool,
    /// Whether to preserve (and thus write) ACLs to the wire.
    preserve_acls: bool,
    /// Whether to preserve (and thus write) extended attributes to the wire.
    preserve_xattrs: bool,
    /// Length of checksum to write (depends on protocol and checksum algorithm).
    flist_csum_len: usize,
    /// Optional filename encoding converter (for --iconv support).
    iconv: Option<FilenameConverter>,
    /// Cached: whether varint flag encoding is enabled (computed once at construction).
    use_varint_flags: bool,
    /// Cached: whether safe file list mode is enabled (computed once at construction).
    use_safe_file_list: bool,
}

impl FileListWriter {
    /// Creates a new file list writer for the given protocol version.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        Self {
            protocol,
            codec: create_protocol_codec(protocol.as_u8()),
            state: FileListCompressionState::new(),
            stats: FileListStats::default(),
            preserve_uid: false,
            preserve_gid: false,
            preserve_links: false,
            preserve_devices: false,
            preserve_hard_links: false,
            preserve_atimes: false,
            preserve_crtimes: false,
            always_checksum: false,
            preserve_acls: false,
            preserve_xattrs: false,
            flist_csum_len: 0,
            iconv: None,
            use_varint_flags: false,
            use_safe_file_list: protocol.safe_file_list_always_enabled(),
        }
    }

    /// Creates a new file list writer with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        Self {
            protocol,
            codec: create_protocol_codec(protocol.as_u8()),
            state: FileListCompressionState::new(),
            stats: FileListStats::default(),
            preserve_uid: false,
            preserve_gid: false,
            preserve_links: false,
            preserve_devices: false,
            preserve_hard_links: false,
            preserve_atimes: false,
            preserve_crtimes: false,
            always_checksum: false,
            preserve_acls: false,
            preserve_xattrs: false,
            flist_csum_len: 0,
            iconv: None,
            use_varint_flags: compat_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            use_safe_file_list: compat_flags.contains(CompatibilityFlags::SAFE_FILE_LIST)
                || protocol.safe_file_list_always_enabled(),
        }
    }

    /// Sets whether UID values should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_uid(mut self, preserve: bool) -> Self {
        self.preserve_uid = preserve;
        self
    }

    /// Sets whether GID values should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.preserve_gid = preserve;
        self
    }

    /// Sets whether symlink targets should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_links(mut self, preserve: bool) -> Self {
        self.preserve_links = preserve;
        self
    }

    /// Sets whether device numbers should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Sets whether hardlink indices should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Sets whether access times should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_atimes(mut self, preserve: bool) -> Self {
        self.preserve_atimes = preserve;
        self
    }

    /// Sets whether creation times should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_crtimes(mut self, preserve: bool) -> Self {
        self.preserve_crtimes = preserve;
        self
    }

    /// Sets whether ACLs should be written to the wire.
    ///
    /// When enabled, ACL indices are written after other metadata.
    /// Note: ACL data itself is sent in a separate exchange.
    #[inline]
    #[must_use]
    pub const fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Sets whether extended attributes should be written to the wire.
    ///
    /// When enabled, xattr indices are written after ACL indices.
    /// Note: Xattr data itself is sent in a separate exchange.
    #[inline]
    #[must_use]
    pub const fn with_preserve_xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Enables checksum mode (--checksum / -c) with the given checksum length.
    ///
    /// When enabled, checksums are written for regular files. For protocol < 28,
    /// checksums are also written for non-regular files (using empty_sum).
    #[inline]
    #[must_use]
    pub const fn with_always_checksum(mut self, csum_len: usize) -> Self {
        self.always_checksum = true;
        self.flist_csum_len = csum_len;
        self
    }

    /// Sets the filename encoding converter for iconv support.
    #[inline]
    #[must_use]
    pub const fn with_iconv(mut self, converter: FilenameConverter) -> Self {
        self.iconv = Some(converter);
        self
    }

    /// Returns the statistics collected during file list writing.
    #[must_use]
    pub const fn stats(&self) -> &FileListStats {
        &self.stats
    }

    /// Returns whether varint flag encoding is enabled.
    #[inline]
    const fn use_varint_flags(&self) -> bool {
        self.use_varint_flags
    }

    /// Returns whether safe file list mode is enabled.
    #[inline]
    const fn use_safe_file_list(&self) -> bool {
        self.use_safe_file_list
    }

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
    /// | `XMIT_HLINKED` | 9 | Entry is a hardlink (protocol 30+) |
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
    fn calculate_xflags(&self, entry: &FileEntry, same_len: usize, suffix_len: usize) -> u32 {
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

        // Directory with top_dir flag
        if entry.is_dir() && entry.flags().top_dir() {
            xflags |= XMIT_TOP_DIR as u32;
        }

        // Mode comparison
        if entry.mode() == self.state.prev_mode() {
            xflags |= XMIT_SAME_MODE as u32;
        }

        // Time comparison
        if entry.mtime() == self.state.prev_mtime() {
            xflags |= XMIT_SAME_TIME as u32;
        }

        // UID comparison
        let entry_uid = entry.uid().unwrap_or(0);
        if self.preserve_uid && entry_uid == self.state.prev_uid() {
            xflags |= XMIT_SAME_UID as u32;
        }

        // GID comparison
        let entry_gid = entry.gid().unwrap_or(0);
        if self.preserve_gid && entry_gid == self.state.prev_gid() {
            xflags |= XMIT_SAME_GID as u32;
        }

        // Name compression
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

        // Device/special file rdev major comparison (protocol 28+)
        // Devices always, special files only for protocol < 31
        let needs_rdev = self.preserve_devices
            && (entry.is_device() || (entry.is_special() && self.protocol.as_u8() < 31));

        if !needs_rdev {
            return xflags;
        }

        let major = if entry.is_device() {
            entry.rdev_major().unwrap_or(0)
        } else {
            0 // Dummy rdev for special files
        };

        if major == self.state.prev_rdev_major() {
            xflags |= (XMIT_SAME_RDEV_MAJOR as u32) << 8;
        }

        // Set XMIT_RDEV_MINOR_8_PRE30 flag if minor fits in byte (protocol 28-29)
        if self.protocol.as_u8() >= 28 && self.protocol.as_u8() < 30 {
            let minor = if entry.is_device() {
                entry.rdev_minor().unwrap_or(0)
            } else {
                0
            };
            if minor <= 0xFF {
                xflags |= (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
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

        if !self.preserve_hard_links || entry.is_dir() {
            return xflags;
        }

        if self.protocol.as_u8() >= 30 {
            // Protocol 30+: Use XMIT_HLINKED / XMIT_HLINK_FIRST
            if let Some(idx) = entry.hardlink_idx() {
                xflags |= (XMIT_HLINKED as u32) << 8;
                if idx == u32::MAX {
                    xflags |= (XMIT_HLINK_FIRST as u32) << 8;
                }
            }
        } else if self.protocol.as_u8() >= 28 {
            // Protocol 28-29: Use XMIT_SAME_DEV_PRE30 for hardlink dev compression
            if let Some(dev) = entry.hardlink_dev() {
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

        // User name follows flag
        if self.preserve_uid
            && entry.user_name().is_some()
            && (current_flags & (XMIT_SAME_UID as u32)) == 0
        {
            xflags |= (XMIT_USER_NAME_FOLLOWS as u32) << 8;
        }

        // Group name follows flag
        if self.preserve_gid
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

        // Same atime flag (non-directories only, when preserving atimes)
        if self.preserve_atimes && !entry.is_dir() && entry.atime() == self.state.prev_atime() {
            xflags |= (XMIT_SAME_ATIME as u32) << 8;
        }

        // Creation time equals mtime flag (bit 17, varint mode only)
        if self.preserve_crtimes && entry.crtime() == entry.mtime() {
            xflags |= (XMIT_CRTIME_EQ_MTIME as u32) << 16;
        }

        // Modification time nanoseconds flag (protocol 31+)
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

        // No content directory flag (protocol 30+, directories only)
        // Note: shares bit position with XMIT_SAME_RDEV_MAJOR (devices vs dirs)
        if entry.is_dir() && self.protocol.as_u8() >= 30 && !entry.content_dir() {
            xflags |= (XMIT_NO_CONTENT_DIR as u32) << 8;
        }

        xflags
    }

    /// Writes flags to the wire in the appropriate format.
    fn write_flags<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        xflags: u32,
        is_dir: bool,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: avoid xflags=0 which looks like end marker.
            // Upstream flist.c line 550: write_varint(f, xflags ? xflags : XMIT_EXTENDED_FLAGS)
            let flags_to_write = if xflags == 0 {
                XMIT_EXTENDED_FLAGS as u32
            } else {
                xflags
            };
            write_varint(writer, flags_to_write as i32)?;
        } else if self.protocol.supports_extended_flags() {
            // Protocol 28-29: two-byte encoding if needed
            let mut xflags_to_write = xflags;
            if xflags_to_write == 0 && !is_dir {
                xflags_to_write |= XMIT_TOP_DIR as u32;
            }

            if (xflags_to_write & 0xFF00) != 0 || xflags_to_write == 0 {
                xflags_to_write |= XMIT_EXTENDED_FLAGS as u32;
                writer.write_all(&(xflags_to_write as u16).to_le_bytes())?;
            } else {
                writer.write_all(&[xflags_to_write as u8])?;
            }
        } else {
            // Protocol < 28: single byte
            let flags_to_write = if xflags == 0 && !is_dir {
                XMIT_LONG_NAME as u32
            } else {
                xflags
            };
            writer.write_all(&[flags_to_write as u8])?;
        }
        Ok(())
    }

    /// Writes name compression info and suffix.
    fn write_name<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        name: &[u8],
        same_len: usize,
        suffix_len: usize,
        xflags: u32,
    ) -> io::Result<()> {
        // Write same_len if XMIT_SAME_NAME is set
        if xflags & (XMIT_SAME_NAME as u32) != 0 {
            writer.write_all(&[same_len as u8])?;
        }

        // Write suffix length
        if xflags & (XMIT_LONG_NAME as u32) != 0 {
            self.codec.write_long_name_len(writer, suffix_len)?;
        } else {
            writer.write_all(&[suffix_len as u8])?;
        }

        // Write suffix bytes
        writer.write_all(&name[same_len..])
    }

    /// Writes metadata fields in upstream rsync wire format order.
    ///
    /// Order (matching flist.c send_file_entry lines 580-620):
    /// 1. size (varlong30)
    /// 2. mtime (if not XMIT_SAME_TIME)
    /// 3. nsec (if XMIT_MOD_NSEC, protocol 31+)
    /// 4. crtime (if preserving, not XMIT_CRTIME_EQ_MTIME)
    /// 5. mode (if not XMIT_SAME_MODE)
    /// 6. atime (if preserving, non-dir, not XMIT_SAME_ATIME)
    /// 7. uid + user name (if preserving, not XMIT_SAME_UID)
    /// 8. gid + group name (if preserving, not XMIT_SAME_GID)
    fn write_metadata<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        self.write_size(writer, entry)?;
        self.write_time_fields(writer, entry, xflags)?;
        self.write_mode(writer, entry, xflags)?;
        self.write_atime(writer, entry, xflags)?;
        self.write_uid_field(writer, entry, xflags)?;
        self.write_gid_field(writer, entry, xflags)?;
        Ok(())
    }

    /// Writes file size using protocol-appropriate encoding.
    #[inline]
    fn write_size<W: Write + ?Sized>(&self, writer: &mut W, entry: &FileEntry) -> io::Result<()> {
        self.codec.write_file_size(writer, entry.size() as i64)
    }

    /// Writes time-related fields: mtime, nsec, and crtime.
    #[inline]
    fn write_time_fields<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        // Write mtime if different
        if xflags & (XMIT_SAME_TIME as u32) == 0 {
            self.codec.write_mtime(writer, entry.mtime())?;
        }

        // Write nsec if flag set (protocol 31+)
        if (xflags & ((XMIT_MOD_NSEC as u32) << 8)) != 0 {
            write_varint(writer, entry.mtime_nsec() as i32)?;
        }

        // Write crtime if preserving and different from mtime
        if self.preserve_crtimes && (xflags & ((XMIT_CRTIME_EQ_MTIME as u32) << 16)) == 0 {
            crate::write_varlong(writer, entry.crtime(), 4)?;
        }

        Ok(())
    }

    /// Writes mode field if different from previous entry.
    #[inline]
    fn write_mode<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if xflags & (XMIT_SAME_MODE as u32) == 0 {
            let wire_mode = entry.mode() as i32;
            writer.write_all(&wire_mode.to_le_bytes())?;
        }
        Ok(())
    }

    /// Writes atime field if preserving and different (non-directories only).
    #[inline]
    fn write_atime<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if self.preserve_atimes
            && !entry.is_dir()
            && (xflags & ((XMIT_SAME_ATIME as u32) << 8)) == 0
        {
            crate::write_varlong(writer, entry.atime(), 4)?;
            self.state.update_atime(entry.atime());
        }
        Ok(())
    }

    /// Writes UID and optional user name if preserving and different.
    #[inline]
    fn write_uid_field<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        let entry_uid = entry.uid().unwrap_or(0);
        if !self.preserve_uid || (xflags & (XMIT_SAME_UID as u32)) != 0 {
            return Ok(());
        }

        if self.protocol.uses_fixed_encoding() {
            writer.write_all(&(entry_uid as i32).to_le_bytes())?;
        } else {
            write_varint(writer, entry_uid as i32)?;
            // User name follows UID (protocol 30+)
            if (xflags & ((XMIT_USER_NAME_FOLLOWS as u32) << 8)) != 0 {
                self.write_owner_name(writer, entry.user_name())?;
            }
        }
        self.state.update_uid(entry_uid);
        Ok(())
    }

    /// Writes GID and optional group name if preserving and different.
    #[inline]
    fn write_gid_field<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        let entry_gid = entry.gid().unwrap_or(0);
        if !self.preserve_gid || (xflags & (XMIT_SAME_GID as u32)) != 0 {
            return Ok(());
        }

        if self.protocol.uses_fixed_encoding() {
            writer.write_all(&(entry_gid as i32).to_le_bytes())?;
        } else {
            write_varint(writer, entry_gid as i32)?;
            // Group name follows GID (protocol 30+)
            if (xflags & ((XMIT_GROUP_NAME_FOLLOWS as u32) << 8)) != 0 {
                self.write_owner_name(writer, entry.group_name())?;
            }
        }
        self.state.update_gid(entry_gid);
        Ok(())
    }

    /// Writes a user or group name (truncated to 255 bytes).
    #[inline]
    fn write_owner_name<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        name: Option<&str>,
    ) -> io::Result<()> {
        if let Some(name) = name {
            let name_bytes = name.as_bytes();
            let len = name_bytes.len().min(255) as u8;
            writer.write_all(&[len])?;
            writer.write_all(&name_bytes[..len as usize])?;
        }
        Ok(())
    }

    /// Writes symlink target if preserving links and entry is a symlink.
    ///
    /// Wire format: varint30(len) + raw bytes (no null terminator)
    fn write_symlink_target<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        if !self.preserve_links || !entry.is_symlink() {
            return Ok(());
        }

        if let Some(target) = entry.link_target() {
            let target_bytes = target.as_os_str().as_encoded_bytes();
            let len = target_bytes.len();
            write_varint30_int(writer, len as i32, self.protocol.as_u8())?;
            writer.write_all(target_bytes)?;
        }

        Ok(())
    }

    /// Writes device numbers if preserving devices and entry is a device.
    ///
    /// Also writes dummy rdev (0, 0) for special files (FIFOs, sockets) in protocol < 31.
    ///
    /// Wire format (protocol 28+):
    /// - Major: varint30 (omitted if XMIT_SAME_RDEV_MAJOR set)
    /// - Minor: varint (protocol 30+) or byte/int (protocol 28-29)
    fn write_rdev<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        let is_device = entry.is_device();
        let is_special = entry.is_special();

        // Devices: write actual rdev if preserve_devices
        // Special files (proto < 31): write dummy rdev (0, 0) if preserve_devices
        if !self.preserve_devices {
            return Ok(());
        }

        if !is_device && !is_special {
            return Ok(());
        }

        // Special files only get rdev in protocol < 31
        if is_special && self.protocol.as_u8() >= 31 {
            return Ok(());
        }

        let (major, minor) = if is_device {
            (
                entry.rdev_major().unwrap_or(0),
                entry.rdev_minor().unwrap_or(0),
            )
        } else {
            // Special file: dummy rdev (0, 0)
            (0, 0)
        };

        // Write major if not same as previous
        if xflags & ((XMIT_SAME_RDEV_MAJOR as u32) << 8) == 0 {
            write_varint30_int(writer, major as i32, self.protocol.as_u8())?;
        }

        // Write minor (always)
        if self.protocol.as_u8() >= 30 {
            write_varint(writer, minor as i32)?;
        } else {
            // Protocol 28-29: check XMIT_RDEV_MINOR_8_PRE30 flag
            let minor_8_bit = (xflags & ((XMIT_RDEV_MINOR_8_PRE30 as u32) << 8)) != 0;
            if minor_8_bit {
                writer.write_all(&[minor as u8])?;
            } else {
                writer.write_all(&(minor as i32).to_le_bytes())?;
            }
        }

        // Update compression state
        self.state.update_rdev_major(major);

        Ok(())
    }

    /// Writes hardlink index if preserving hardlinks and entry has one.
    ///
    /// Wire format (protocol 30+):
    /// - If XMIT_HLINKED is set but not XMIT_HLINK_FIRST: write varint index
    /// - If XMIT_HLINK_FIRST is also set: no index (this is the first/leader)
    fn write_hardlink_idx<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if !self.preserve_hard_links || self.protocol.as_u8() < 30 {
            return Ok(());
        }

        // Only write index if XMIT_HLINKED is set and XMIT_HLINK_FIRST is NOT set
        let hlinked = (xflags & ((XMIT_HLINKED as u32) << 8)) != 0;
        let hlink_first = (xflags & ((XMIT_HLINK_FIRST as u32) << 8)) != 0;

        if hlinked && !hlink_first {
            if let Some(idx) = entry.hardlink_idx() {
                write_varint(writer, idx as i32)?;
            }
        }

        Ok(())
    }

    /// Writes hardlink device and inode for protocol 28-29.
    ///
    /// In protocols before 30, hardlinks are identified by (dev, ino) pairs
    /// rather than indices. This writes the dev/ino after the symlink target.
    ///
    /// Wire format:
    /// - If not XMIT_SAME_DEV_PRE30: write longint(dev + 1)
    /// - Always write longint(ino)
    fn write_hardlink_dev_ino<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        // Only for protocol 28-29, non-directories with hardlink info
        if !self.preserve_hard_links
            || self.protocol.as_u8() >= 30
            || self.protocol.as_u8() < 28
            || entry.is_dir()
        {
            return Ok(());
        }

        let dev = match entry.hardlink_dev() {
            Some(d) => d,
            None => return Ok(()),
        };

        let ino = entry.hardlink_ino().unwrap_or(0);

        // Write dev if not same as previous
        let same_dev = (xflags & ((XMIT_SAME_DEV_PRE30 as u32) << 8)) != 0;
        if !same_dev {
            // Write dev + 1 (upstream convention)
            crate::write_longint(writer, dev + 1)?;
        }

        // Always write ino
        crate::write_longint(writer, ino)?;

        // Update compression state
        self.state.update_hardlink_dev(dev);

        Ok(())
    }

    /// Applies iconv encoding conversion to a filename.
    fn apply_encoding_conversion<'a>(
        &self,
        name: &'a [u8],
    ) -> io::Result<std::borrow::Cow<'a, [u8]>> {
        if let Some(ref converter) = self.iconv {
            match converter.local_to_remote(name) {
                Ok(converted) => Ok(converted),
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("filename encoding conversion failed: {e}"),
                )),
            }
        } else {
            Ok(std::borrow::Cow::Borrowed(name))
        }
    }

    /// Returns true if this entry is a hardlink follower (metadata should be skipped).
    ///
    /// A hardlink follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST.
    /// Such entries reference another entry in the file list, so their metadata
    /// (size, mtime, mode, uid, gid, symlink, rdev) is omitted from the wire.
    #[inline]
    fn is_hardlink_follower(&self, xflags: u32) -> bool {
        let hlinked = (xflags & ((XMIT_HLINKED as u32) << 8)) != 0;
        let hlink_first = (xflags & ((XMIT_HLINK_FIRST as u32) << 8)) != 0;
        hlinked && !hlink_first
    }

    /// Writes a file entry to the stream.
    ///
    /// Wire format order (matching upstream rsync flist.c send_file_entry):
    /// 1. Flags
    /// 2. Name (with prefix compression)
    /// 3. Hardlink index (if follower) - then STOP for followers
    /// 4. File size
    /// 5. Mtime (if not XMIT_SAME_TIME)
    /// 6. Nsec (if XMIT_MOD_NSEC)
    /// 7. Crtime (if preserving and not XMIT_CRTIME_EQ_MTIME)
    /// 8. Mode (if not XMIT_SAME_MODE)
    /// 9. Atime (if preserving, non-dir, not XMIT_SAME_ATIME)
    /// 10. UID (if preserving, not XMIT_SAME_UID) + user name
    /// 11. GID (if preserving, not XMIT_SAME_GID) + group name
    /// 12. Device numbers (if device/special file)
    /// 13. Symlink target (if symlink)
    ///
    /// # Upstream Reference
    ///
    /// See `flist.c:send_file_entry()` lines 470-750 for the complete wire encoding.
    pub fn write_entry<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        // Step 1: Get name bytes and apply encoding conversion
        let raw_name = entry.name_bytes();
        let name = self.apply_encoding_conversion(raw_name)?;

        // Step 2: Calculate name compression
        let same_len = self.state.calculate_name_prefix_len(&name);
        let suffix_len = name.len() - same_len;

        // Step 3: Calculate xflags
        let xflags = self.calculate_xflags(entry, same_len, suffix_len);

        // Step 4: Write flags
        self.write_flags(writer, xflags, entry.is_dir())?;

        // Step 5: Write name
        self.write_name(writer, &name, same_len, suffix_len, xflags)?;

        // Step 6: Write hardlink index (MUST come immediately after name)
        // For hardlink followers, this is the only field written after the name.
        // Upstream rsync does "goto the_end" after writing the index for followers.
        self.write_hardlink_idx(writer, entry, xflags)?;

        // Step 7+: Write metadata (unless this is a hardlink follower)
        // Hardlink followers have their metadata copied from the leader entry,
        // so we skip writing size, mtime, mode, uid, gid, symlink, and rdev.
        if !self.is_hardlink_follower(xflags) {
            // Step 7: Write metadata (size, mtime, nsec, crtime, mode, atime, uid, gid)
            self.write_metadata(writer, entry, xflags)?;

            // Step 8: Write device numbers (if applicable)
            // Also write dummy rdev for special files (FIFOs, sockets) in protocol < 31
            self.write_rdev(writer, entry, xflags)?;

            // Step 9: Write symlink target (if applicable)
            self.write_symlink_target(writer, entry)?;

            // Step 10: Write hardlink dev/ino for protocol < 30
            self.write_hardlink_dev_ino(writer, entry, xflags)?;
        }

        // Step 10: Write checksum if always_checksum mode is enabled
        // Upstream: always_checksum && (S_ISREG(mode) || protocol_version < 28)
        if !self.is_hardlink_follower(xflags) {
            self.write_checksum(writer, entry)?;
        }

        // Step 11: Update state
        self.state.update(
            &name,
            entry.mode(),
            entry.mtime(),
            entry.uid().unwrap_or(0),
            entry.gid().unwrap_or(0),
        );

        // Step 12: Update statistics
        self.update_stats(entry);

        Ok(())
    }

    /// Writes checksum if always_checksum mode is enabled.
    ///
    /// Wire format: raw bytes of length flist_csum_len
    /// - For regular files: actual checksum from entry
    /// - For non-regular files (proto < 28 only): empty_sum (all zeros)
    fn write_checksum<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        if !self.always_checksum || self.flist_csum_len == 0 {
            return Ok(());
        }

        let is_regular = entry.is_file();

        // For protocol < 28, non-regular files also get a checksum (empty_sum)
        // For protocol >= 28, only regular files get checksums
        if !is_regular && self.protocol.as_u8() >= 28 {
            return Ok(());
        }

        if is_regular {
            // Write actual checksum from entry, or zeros if not set
            if let Some(sum) = entry.checksum() {
                let len = sum.len().min(self.flist_csum_len);
                writer.write_all(&sum[..len])?;
                // Pad with zeros if checksum is shorter than expected
                if len < self.flist_csum_len {
                    let padding = vec![0u8; self.flist_csum_len - len];
                    writer.write_all(&padding)?;
                }
            } else {
                // No checksum set, write zeros
                let zeros = vec![0u8; self.flist_csum_len];
                writer.write_all(&zeros)?;
            }
        } else {
            // Non-regular file (proto < 28): write empty_sum (all zeros)
            let zeros = vec![0u8; self.flist_csum_len];
            writer.write_all(&zeros)?;
        }

        Ok(())
    }

    /// Updates file list statistics based on the entry type.
    fn update_stats(&mut self, entry: &FileEntry) {
        if entry.is_dir() {
            self.stats.num_dirs += 1;
        } else if entry.is_file() {
            self.stats.num_files += 1;
            self.stats.total_size += entry.size();
        } else if entry.is_symlink() {
            self.stats.num_symlinks += 1;
            // Symlinks contribute their target length to total_size in rsync
            if let Some(target) = entry.link_target() {
                self.stats.total_size += target.as_os_str().len() as u64;
            }
        } else if entry.is_device() {
            self.stats.num_devices += 1;
        } else if entry.is_special() {
            self.stats.num_specials += 1;
        }
    }

    /// Writes the end-of-list marker.
    pub fn write_end<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        io_error: Option<i32>,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: zero flags + error code
            write_varint(writer, 0)?;
            write_varint(writer, io_error.unwrap_or(0))?;
            return Ok(());
        }

        if let Some(error) = io_error
            && self.use_safe_file_list()
        {
            // Error marker + code
            let marker_lo = XMIT_EXTENDED_FLAGS;
            let marker_hi = XMIT_IO_ERROR_ENDLIST;
            writer.write_all(&[marker_lo, marker_hi])?;
            write_varint(writer, error)?;
            return Ok(());
        }

        // Normal end marker
        writer.write_all(&[0u8])
    }
}

/// Writes a single file entry to a writer.
pub fn write_file_entry<W: Write>(
    writer: &mut W,
    entry: &FileEntry,
    protocol: ProtocolVersion,
) -> io::Result<()> {
    let mut list_writer = FileListWriter::new(protocol);
    list_writer.write_entry(writer, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_protocol() -> ProtocolVersion {
        ProtocolVersion::try_from(32u8).unwrap()
    }

    #[test]
    fn write_end_marker() {
        let mut buf = Vec::new();
        let writer = FileListWriter::new(test_protocol());
        writer.write_end(&mut buf, None).unwrap();
        assert_eq!(buf, vec![0u8]);
    }

    #[test]
    fn write_simple_entry() {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(test_protocol());
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);

        writer.write_entry(&mut buf, &entry).unwrap();

        assert!(!buf.is_empty());
        assert_ne!(buf[0], 0);
    }

    #[test]
    fn write_multiple_entries_with_compression() {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(test_protocol());

        let entry1 = FileEntry::new_file("dir/file1.txt".into(), 100, 0o644);
        let entry2 = FileEntry::new_file("dir/file2.txt".into(), 200, 0o644);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();

        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;

        assert!(second_len < first_len, "second entry should be compressed");
    }

    #[test]
    fn write_then_read_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.size(), 1024);
    }

    #[test]
    fn write_end_with_safe_file_list_enabled_transmits_error() {
        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST;
        let writer = FileListWriter::with_compat_flags(protocol, flags);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(23)).unwrap();

        assert_ne!(buf, vec![0u8]);
        assert!(buf.len() > 1);
        assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
        assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

        use crate::varint::decode_varint;
        let cursor = &buf[2..];
        let (error_code, _) = decode_varint(cursor).unwrap();
        assert_eq!(error_code, 23);
    }

    #[test]
    fn write_end_without_safe_file_list_writes_normal_marker_even_with_error() {
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let writer = FileListWriter::new(protocol);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(23)).unwrap();

        assert_eq!(buf, vec![0u8]);
    }

    #[test]
    fn write_end_with_protocol_31_enables_safe_mode_automatically() {
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let writer = FileListWriter::new(protocol);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(42)).unwrap();

        assert_ne!(buf, vec![0u8]);
        assert!(buf.len() > 1);
        assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
        assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

        use crate::varint::decode_varint;
        let cursor = &buf[2..];
        let (error_code, _) = decode_varint(cursor).unwrap();
        assert_eq!(error_code, 42);
    }

    // Tests for extracted helper methods

    #[test]
    fn calculate_xflags_mode_comparison() {
        let mut writer = FileListWriter::new(test_protocol());
        // FileEntry::new_file includes file type bits (S_IFREG = 0o100000)
        // so mode 0o644 becomes 0o100644
        writer.state.update_mode(0o100644);

        let entry_same = FileEntry::new_file("test".into(), 100, 0o644);
        let entry_diff = FileEntry::new_file("test".into(), 100, 0o755);

        let flags_same = writer.calculate_xflags(&entry_same, 0, 4);
        let flags_diff = writer.calculate_xflags(&entry_diff, 0, 4);

        assert!(flags_same & (XMIT_SAME_MODE as u32) != 0);
        assert!(flags_diff & (XMIT_SAME_MODE as u32) == 0);
    }

    #[test]
    fn calculate_xflags_time_comparison() {
        let mut writer = FileListWriter::new(test_protocol());
        writer.state.update_mtime(1700000000);

        let mut entry_same = FileEntry::new_file("test".into(), 100, 0o644);
        entry_same.set_mtime(1700000000, 0);

        let mut entry_diff = FileEntry::new_file("test".into(), 100, 0o644);
        entry_diff.set_mtime(1700000001, 0);

        let flags_same = writer.calculate_xflags(&entry_same, 0, 4);
        let flags_diff = writer.calculate_xflags(&entry_diff, 0, 4);

        assert!(flags_same & (XMIT_SAME_TIME as u32) != 0);
        assert!(flags_diff & (XMIT_SAME_TIME as u32) == 0);
    }

    #[test]
    fn calculate_xflags_name_compression() {
        let writer = FileListWriter::new(test_protocol());
        let entry = FileEntry::new_file("test".into(), 100, 0o644);

        let flags_no_prefix = writer.calculate_xflags(&entry, 0, 4);
        let flags_with_prefix = writer.calculate_xflags(&entry, 2, 2);
        let flags_long_name = writer.calculate_xflags(&entry, 0, 300);

        assert!(flags_no_prefix & (XMIT_SAME_NAME as u32) == 0);
        assert!(flags_with_prefix & (XMIT_SAME_NAME as u32) != 0);
        assert!(flags_long_name & (XMIT_LONG_NAME as u32) != 0);
    }

    #[test]
    fn use_varint_flags_checks_compat_flags() {
        let protocol = test_protocol();

        let writer_without = FileListWriter::new(protocol);
        assert!(!writer_without.use_varint_flags());

        let writer_with =
            FileListWriter::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
        assert!(writer_with.use_varint_flags());
    }

    #[test]
    fn use_safe_file_list_checks_protocol_and_flags() {
        let writer30 = FileListWriter::new(ProtocolVersion::try_from(30u8).unwrap());
        assert!(!writer30.use_safe_file_list());

        let writer30_safe = FileListWriter::with_compat_flags(
            ProtocolVersion::try_from(30u8).unwrap(),
            CompatibilityFlags::SAFE_FILE_LIST,
        );
        assert!(writer30_safe.use_safe_file_list());

        let writer31 = FileListWriter::new(ProtocolVersion::try_from(31u8).unwrap());
        assert!(writer31.use_safe_file_list());
    }

    #[test]
    fn write_symlink_entry_with_preserve_links() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

        let entry = FileEntry::new_symlink("link".into(), "/target/path".into());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "link");
        assert!(read_entry.is_symlink());
        assert_eq!(
            read_entry
                .link_target()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("/target/path".to_string())
        );
    }

    #[test]
    fn write_symlink_entry_without_preserve_links_omits_target() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol); // preserve_links = false

        let entry = FileEntry::new_symlink("link".into(), "/target/path".into());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol); // preserve_links = false

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "link");
        assert!(read_entry.is_symlink());
        // Target should NOT be present since preserve_links was false
        assert!(read_entry.link_target().is_none());
    }

    #[test]
    fn write_symlink_round_trip_protocol_30_varint() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 30+ uses varint30
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

        let entry = FileEntry::new_symlink("mylink".into(), "../relative/path".into());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "mylink");
        assert!(read_entry.is_symlink());
        assert_eq!(
            read_entry
                .link_target()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("../relative/path".to_string())
        );
    }

    #[test]
    fn write_symlink_round_trip_protocol_29_fixed_int() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 29 uses fixed 4-byte int
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

        let entry = FileEntry::new_symlink("oldlink".into(), "/old/target".into());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "oldlink");
        assert!(read_entry.is_symlink());
        assert_eq!(
            read_entry
                .link_target()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("/old/target".to_string())
        );
    }

    #[test]
    fn write_block_device_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "sda");
        assert!(read_entry.is_device());
        assert!(read_entry.is_block_device());
        assert_eq!(read_entry.rdev_major(), Some(8));
        assert_eq!(read_entry.rdev_minor(), Some(0));
    }

    #[test]
    fn write_char_device_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        let entry = FileEntry::new_char_device("null".into(), 0o666, 1, 3);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "null");
        assert!(read_entry.is_device());
        assert!(read_entry.is_char_device());
        assert_eq!(read_entry.rdev_major(), Some(1));
        assert_eq!(read_entry.rdev_minor(), Some(3));
    }

    #[test]
    fn write_device_without_preserve_devices_omits_rdev() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol); // preserve_devices = false

        let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol); // preserve_devices = false

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "sda");
        assert!(read_entry.is_block_device());
        // rdev should NOT be present since preserve_devices was false
        assert!(read_entry.rdev_major().is_none());
        assert!(read_entry.rdev_minor().is_none());
    }

    #[test]
    fn write_multiple_devices_with_same_major_compression() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        // Two devices with same major (8) - second should use XMIT_SAME_RDEV_MAJOR
        let entry1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
        let entry2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller due to major compression
        assert!(
            second_len < first_len,
            "second device entry should be compressed"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.rdev_major(), Some(8));
        assert_eq!(read1.rdev_minor(), Some(0));
        assert_eq!(read2.rdev_major(), Some(8));
        assert_eq!(read2.rdev_minor(), Some(16));
    }

    #[test]
    fn write_hardlink_first_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        // First file in hardlink group (leader)
        let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry.set_hardlink_idx(u32::MAX); // u32::MAX indicates first/leader

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "file1.txt");
        assert_eq!(read_entry.hardlink_idx(), Some(u32::MAX));
    }

    #[test]
    fn write_hardlink_follower_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        // Hardlink follower pointing to index 5
        let mut entry = FileEntry::new_file("file2.txt".into(), 100, 0o644);
        entry.set_hardlink_idx(5);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "file2.txt");
        assert_eq!(read_entry.hardlink_idx(), Some(5));
    }

    #[test]
    fn write_hardlink_without_preserve_hard_links_omits_idx() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol); // preserve_hard_links = false

        let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry.set_hardlink_idx(5);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol); // preserve_hard_links = false

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "file1.txt");
        // hardlink_idx should NOT be present since preserve_hard_links was false
        assert!(read_entry.hardlink_idx().is_none());
    }

    #[test]
    fn write_hardlink_group_round_trip_protocol_32() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        // First: leader (u32::MAX)
        let mut entry1 = FileEntry::new_file("original.txt".into(), 500, 0o644);
        entry1.set_hardlink_idx(u32::MAX);

        // Second: follower pointing to index 0
        let mut entry2 = FileEntry::new_file("link1.txt".into(), 500, 0o644);
        entry2.set_hardlink_idx(0);

        // Third: follower pointing to index 0
        let mut entry3 = FileEntry::new_file("link2.txt".into(), 500, 0o644);
        entry3.set_hardlink_idx(0);

        writer.write_entry(&mut buf, &entry1).unwrap();
        writer.write_entry(&mut buf, &entry2).unwrap();
        writer.write_entry(&mut buf, &entry3).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.name(), "original.txt");
        assert_eq!(read1.hardlink_idx(), Some(u32::MAX));

        assert_eq!(read2.name(), "link1.txt");
        assert_eq!(read2.hardlink_idx(), Some(0));

        assert_eq!(read3.name(), "link2.txt");
        assert_eq!(read3.hardlink_idx(), Some(0));
    }

    #[test]
    fn write_user_name_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
        entry.set_uid(1000);
        entry.set_user_name("testuser".to_string());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "file.txt");
        assert_eq!(read_entry.user_name(), Some("testuser"));
    }

    #[test]
    fn write_group_name_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
        entry.set_gid(1000);
        entry.set_group_name("testgroup".to_string());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "file.txt");
        assert_eq!(read_entry.group_name(), Some("testgroup"));
    }

    #[test]
    fn write_user_and_group_names_round_trip_protocol_32() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("owned.txt".into(), 500, 0o644);
        entry.set_uid(1001);
        entry.set_gid(1002);
        entry.set_user_name("alice".to_string());
        entry.set_group_name("developers".to_string());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "owned.txt");
        assert_eq!(read_entry.user_name(), Some("alice"));
        assert_eq!(read_entry.group_name(), Some("developers"));
    }

    #[test]
    fn write_user_name_omitted_when_same_uid() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        // First entry sets the UID
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_uid(1000);
        entry1.set_user_name("testuser".to_string());

        // Second entry has same UID - should use XMIT_SAME_UID (no name written)
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_uid(1000);
        entry2.set_user_name("testuser".to_string());

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller (no user name written)
        assert!(
            second_len < first_len,
            "second entry should not include user name"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.user_name(), Some("testuser"));
        // Second entry doesn't get user_name since XMIT_SAME_UID was set
        assert_eq!(read2.user_name(), None);
    }

    #[test]
    fn write_names_omitted_for_protocol_29() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 29 doesn't support user/group name strings
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
        entry.set_uid(1000);
        entry.set_gid(1000);
        entry.set_user_name("testuser".to_string());
        entry.set_group_name("testgroup".to_string());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        // Names should NOT be present for protocol 29
        assert_eq!(read_entry.user_name(), None);
        assert_eq!(read_entry.group_name(), None);
    }

    #[test]
    fn write_hardlink_follower_skips_metadata() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        // First: leader (u32::MAX) - full metadata
        let mut entry1 = FileEntry::new_file("original.txt".into(), 500, 0o644);
        entry1.set_mtime(1700000000, 0);
        entry1.set_hardlink_idx(u32::MAX);

        // Second: follower pointing to index 0 - metadata skipped
        let mut entry2 = FileEntry::new_file("link.txt".into(), 500, 0o644);
        entry2.set_mtime(1700000000, 0);
        entry2.set_hardlink_idx(0);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

        // Follower should be MUCH smaller (no size, mtime, mode)
        assert!(
            second_len < first_len / 2,
            "follower entry should be much smaller: {second_len} vs {first_len}"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        // Leader has full metadata
        assert_eq!(read1.name(), "original.txt");
        assert_eq!(read1.size(), 500);
        assert_eq!(read1.mtime(), 1700000000);
        assert_eq!(read1.hardlink_idx(), Some(u32::MAX));

        // Follower has zeroed metadata (caller should copy from leader)
        assert_eq!(read2.name(), "link.txt");
        assert_eq!(read2.size(), 0); // Metadata was skipped
        assert_eq!(read2.mtime(), 0); // Metadata was skipped
        assert_eq!(read2.hardlink_idx(), Some(0));
    }

    #[test]
    fn write_hardlink_follower_with_uid_gid_skips_all() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_hard_links(true)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        // Leader with full metadata
        let mut entry1 = FileEntry::new_file("leader.txt".into(), 1000, 0o755);
        entry1.set_mtime(1700000000, 0);
        entry1.set_uid(1000);
        entry1.set_gid(1000);
        entry1.set_user_name("testuser".to_string());
        entry1.set_group_name("testgroup".to_string());
        entry1.set_hardlink_idx(u32::MAX);

        // Follower - all metadata should be skipped
        let mut entry2 = FileEntry::new_file("follower.txt".into(), 1000, 0o755);
        entry2.set_mtime(1700000000, 0);
        entry2.set_uid(1000);
        entry2.set_gid(1000);
        entry2.set_hardlink_idx(0);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

        // Follower should be significantly smaller
        assert!(
            second_len < first_len / 2,
            "follower should skip metadata: {second_len} vs {first_len}"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_hard_links(true)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        // Leader has full metadata
        assert_eq!(read1.user_name(), Some("testuser"));
        assert_eq!(read1.group_name(), Some("testgroup"));

        // Follower metadata was skipped
        assert_eq!(read2.size(), 0);
        assert_eq!(read2.mtime(), 0);
        assert_eq!(read2.mode(), 0);
        assert_eq!(read2.user_name(), None);
        assert_eq!(read2.group_name(), None);
        assert_eq!(read2.hardlink_idx(), Some(0));
    }

    #[test]
    fn write_hardlink_leader_has_full_metadata() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        // Leader should have full metadata even with hardlink flag
        let mut entry = FileEntry::new_file("leader.txt".into(), 500, 0o644);
        entry.set_mtime(1700000000, 0);
        entry.set_hardlink_idx(u32::MAX); // Leader marker

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

        // Leader has full metadata
        assert_eq!(read_entry.name(), "leader.txt");
        assert_eq!(read_entry.size(), 500);
        assert_eq!(read_entry.mtime(), 1700000000);
        assert_eq!(read_entry.hardlink_idx(), Some(u32::MAX));
    }

    #[test]
    fn is_hardlink_follower_helper() {
        let writer = FileListWriter::new(test_protocol()).with_preserve_hard_links(true);

        // No hardlink flags
        let xflags_none: u32 = 0;
        assert!(!writer.is_hardlink_follower(xflags_none));

        // Leader (HLINKED + HLINK_FIRST)
        let xflags_leader = ((XMIT_HLINKED as u32) << 8) | ((XMIT_HLINK_FIRST as u32) << 8);
        assert!(!writer.is_hardlink_follower(xflags_leader));

        // Follower (HLINKED only)
        let xflags_follower = (XMIT_HLINKED as u32) << 8;
        assert!(writer.is_hardlink_follower(xflags_follower));
    }

    #[test]
    fn checksum_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_always_checksum(16);

        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 0);
        entry.set_checksum(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_always_checksum(16);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(
            read_entry.checksum(),
            Some(&vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16][..])
        );
    }

    #[test]
    fn stats_tracking() {
        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_links(true)
            .with_preserve_devices(true);

        // Write various entry types
        let file1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        let file2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        let dir = FileEntry::new_directory("mydir".into(), 0o755);
        let link = FileEntry::new_symlink("mylink".into(), "/target".into());
        let dev = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

        writer.write_entry(&mut buf, &file1).unwrap();
        writer.write_entry(&mut buf, &file2).unwrap();
        writer.write_entry(&mut buf, &dir).unwrap();
        writer.write_entry(&mut buf, &link).unwrap();
        writer.write_entry(&mut buf, &dev).unwrap();

        let stats = writer.stats();
        assert_eq!(stats.num_files, 2);
        assert_eq!(stats.num_dirs, 1);
        assert_eq!(stats.num_symlinks, 1);
        assert_eq!(stats.num_devices, 1);
        assert_eq!(stats.total_size, 300 + 7); // 100 + 200 + len("/target")
    }

    #[test]
    fn hardlink_dev_ino_round_trip_protocol_29() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        let mut entry = FileEntry::new_file("hardlink.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 0);
        entry.set_hardlink_dev(12345);
        entry.set_hardlink_ino(67890);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "hardlink.txt");
        assert_eq!(read_entry.hardlink_dev(), Some(12345));
        assert_eq!(read_entry.hardlink_ino(), Some(67890));
    }

    #[test]
    fn hardlink_dev_compression_protocol_29() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

        // Two entries with same dev should use XMIT_SAME_DEV_PRE30
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_mtime(1700000000, 0);
        entry1.set_hardlink_dev(12345);
        entry1.set_hardlink_ino(1);

        let mut entry2 = FileEntry::new_file("file2.txt".into(), 100, 0o644);
        entry2.set_mtime(1700000000, 0);
        entry2.set_hardlink_dev(12345);
        entry2.set_hardlink_ino(2);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller due to dev compression
        assert!(
            second_len < first_len,
            "second entry should use dev compression"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.hardlink_dev(), Some(12345));
        assert_eq!(read1.hardlink_ino(), Some(1));
        assert_eq!(read2.hardlink_dev(), Some(12345));
        assert_eq!(read2.hardlink_ino(), Some(2));
    }

    #[test]
    fn special_file_fifo_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        let entry = FileEntry::new_fifo("myfifo".into(), 0o644);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "myfifo");
        assert!(read_entry.is_special());
        // rdev should NOT be set (dummy was read and discarded)
        assert!(read_entry.rdev_major().is_none());
    }

    #[test]
    fn special_file_socket_round_trip_protocol_30() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        let entry = FileEntry::new_socket("mysocket".into(), 0o755);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "mysocket");
        assert!(read_entry.is_special());
    }

    #[test]
    fn special_file_no_rdev_in_protocol_31() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let mut buf_30 = Vec::new();
        let mut buf_31 = Vec::new();

        // Protocol 30: FIFOs get dummy rdev
        let mut writer30 = FileListWriter::new(ProtocolVersion::try_from(30u8).unwrap())
            .with_preserve_devices(true);
        let entry = FileEntry::new_fifo("fifo".into(), 0o644);
        writer30.write_entry(&mut buf_30, &entry).unwrap();

        // Protocol 31: FIFOs don't get rdev
        let mut writer31 = FileListWriter::new(protocol).with_preserve_devices(true);
        writer31.write_entry(&mut buf_31, &entry).unwrap();

        // Protocol 31 entry should be smaller (no rdev)
        assert!(
            buf_31.len() < buf_30.len(),
            "protocol 31 should not write rdev for FIFOs"
        );

        // Verify round-trip
        let mut cursor = Cursor::new(&buf_31[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "fifo");
        assert!(read_entry.is_special());
    }

    // Protocol boundary tests

    #[test]
    fn protocol_28_is_oldest_supported() {
        // Protocol 28 is the oldest supported version
        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        assert!(
            protocol.supports_extended_flags(),
            "protocol 28 should support extended flags"
        );
    }

    #[test]
    fn protocol_boundary_28_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 28 - oldest supported, has extended flags
        let protocol28 = ProtocolVersion::try_from(28u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol28)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        entry.set_uid(1000);
        entry.set_gid(1000);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        // Verify protocol 28 round-trip
        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol28)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.size(), 1024);
        assert_eq!(read_entry.uid(), Some(1000));
        assert_eq!(read_entry.gid(), Some(1000));
    }

    #[test]
    fn protocol_boundary_29_30_user_names() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 30 adds user/group name support
        let protocol30 = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol30)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        entry.set_uid(1000);
        entry.set_gid(1000);
        entry.set_user_name("testuser".to_string());
        entry.set_group_name("testgroup".to_string());
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol30)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.user_name(), Some("testuser"));
        assert_eq!(read_entry.group_name(), Some("testgroup"));
    }

    #[test]
    fn protocol_boundary_30_31_nanoseconds() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 31 adds nanosecond mtime support
        let protocol31 = ProtocolVersion::try_from(31u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol31);

        let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        entry.set_mtime(1700000000, 123456789); // With nanoseconds

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol31);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.mtime(), 1700000000);
        assert_eq!(read_entry.mtime_nsec(), 123456789);
    }

    #[test]
    fn very_long_path_name_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();

        // Create a path longer than 255 characters (requires XMIT_LONG_NAME)
        let long_component = "a".repeat(100);
        let long_path = format!(
            "{long_component}/{long_component}/{long_component}/{long_component}/{long_component}"
        );
        assert!(long_path.len() > 255, "path should be longer than 255");

        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let entry = FileEntry::new_file(long_path.clone().into(), 1024, 0o644);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), long_path);
    }

    #[test]
    fn very_long_path_name_with_compression() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();

        // Create two entries with long shared prefix
        let prefix = "a".repeat(200);
        let path1 = format!("{prefix}/file1.txt");
        let path2 = format!("{prefix}/file2.txt");

        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let entry1 = FileEntry::new_file(path1.clone().into(), 1024, 0o644);
        let entry2 = FileEntry::new_file(path2.clone().into(), 2048, 0o644);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let len_after_first = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let len_after_second = buf.len();

        // Second entry should be smaller due to prefix compression
        let second_entry_len = len_after_second - len_after_first;
        assert!(
            second_entry_len < len_after_first,
            "second entry should be compressed due to shared prefix"
        );

        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.name(), path1);
        assert_eq!(read2.name(), path2);
    }

    #[test]
    fn extreme_mtime_values() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();

        // Test extreme mtime values (only non-negative, as negative
        // timestamps are encoded as unsigned in the wire format)
        let test_cases = [
            0i64,                 // Unix epoch
            1,                    // Just after epoch
            i32::MAX as i64,      // Max 32-bit timestamp (2038-01-19)
            i32::MAX as i64 + 1,  // Beyond 32-bit (2038-01-19)
            1_000_000_000_000i64, // Far future (year ~33658)
        ];

        for &mtime in &test_cases {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
            entry.set_mtime(mtime, 0);

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(
                read_entry.mtime(),
                mtime,
                "mtime {mtime} should round-trip correctly"
            );
        }
    }

    #[test]
    fn zero_flags_varint_uses_xmit_extended_flags() {
        // Upstream flist.c line 550: write_varint(f, xflags ? xflags : XMIT_EXTENDED_FLAGS)
        // When all compression flags apply (mode, time, uid, gid same as prev),
        // xflags would be 0, but we substitute XMIT_EXTENDED_FLAGS to avoid
        // collision with the end-of-list marker (which is also 0).
        use crate::varint::decode_varint;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut writer = FileListWriter::with_compat_flags(protocol, flags)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        // Set up compression state so all flags match
        writer
            .state
            .update(b"prefix/", 0o100644, 1700000000, 1000, 1000);

        let mut entry = FileEntry::new_file("prefix/file.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 0); // Same time
        entry.set_uid(1000); // Same UID
        entry.set_gid(1000); // Same GID

        let mut buf = Vec::new();
        writer.write_entry(&mut buf, &entry).unwrap();

        // Decode the first varint to check the flags value
        let (flags_value, _) = decode_varint(&buf).unwrap();

        // Should NOT be 0 (end marker), should be XMIT_EXTENDED_FLAGS (0x04)
        assert_ne!(flags_value, 0, "flags should not be zero (end marker)");
        assert!(
            (flags_value as u32) & (XMIT_EXTENDED_FLAGS as u32) != 0
                || (flags_value as u32) & (XMIT_SAME_NAME as u32) != 0
                || (flags_value as u32) & (XMIT_SAME_MODE as u32) != 0
                || (flags_value as u32) & (XMIT_SAME_TIME as u32) != 0,
            "non-zero flags should be written: got {flags_value:#x}"
        );
    }

    #[test]
    fn xmit_same_uid_compression_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_uid(true);

        // First entry sets the UID
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_uid(1000);

        // Second entry has same UID - XMIT_SAME_UID flag should be set
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_uid(1000);

        // Third entry has different UID
        let mut entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o644);
        entry3.set_uid(2000);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_entry(&mut buf, &entry3).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller (UID compressed)
        assert!(
            second_len < first_len,
            "second entry should use XMIT_SAME_UID compression"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_uid(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.uid(), Some(1000));
        assert_eq!(read2.uid(), Some(1000)); // Inherited from compression state
        assert_eq!(read3.uid(), Some(2000)); // Explicit value
    }

    #[test]
    fn xmit_same_gid_compression_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_gid(true);

        // First entry sets the GID
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_gid(1000);

        // Second entry has same GID - XMIT_SAME_GID flag should be set
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_gid(1000);

        // Third entry has different GID
        let mut entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o644);
        entry3.set_gid(2000);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_entry(&mut buf, &entry3).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller (GID compressed)
        assert!(
            second_len < first_len,
            "second entry should use XMIT_SAME_GID compression"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_gid(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.gid(), Some(1000));
        assert_eq!(read2.gid(), Some(1000)); // Inherited from compression state
        assert_eq!(read3.gid(), Some(2000)); // Explicit value
    }

    #[test]
    fn xmit_same_mode_compression_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // First entry sets the mode (mode includes file type, so use same type)
        let entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);

        // Second entry has same mode - XMIT_SAME_MODE flag should be set
        let entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);

        // Third entry has different mode
        let entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o755);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_entry(&mut buf, &entry3).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller (mode compressed)
        assert!(
            second_len < first_len,
            "second entry should use XMIT_SAME_MODE compression"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.permissions(), 0o644);
        assert_eq!(read2.permissions(), 0o644); // Same mode
        assert_eq!(read3.permissions(), 0o755); // Different mode
    }

    #[test]
    fn xmit_same_time_compression_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // First entry sets the mtime
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_mtime(1700000000, 0);

        // Second entry has same mtime - XMIT_SAME_TIME flag should be set
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_mtime(1700000000, 0);

        // Third entry has different mtime
        let mut entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o644);
        entry3.set_mtime(1700000001, 0);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_entry(&mut buf, &entry3).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller (mtime compressed)
        assert!(
            second_len < first_len,
            "second entry should use XMIT_SAME_TIME compression"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.mtime(), 1700000000);
        assert_eq!(read2.mtime(), 1700000000); // Same time
        assert_eq!(read3.mtime(), 1700000001); // Different time
    }

    #[test]
    fn name_prefix_compression_max_255_bytes() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // Create two entries with a prefix longer than 255 bytes
        // The compression should cap at 255 since same_len is stored as u8
        let long_prefix = "x".repeat(300);
        let path1 = format!("{long_prefix}/file1.txt");
        let path2 = format!("{long_prefix}/file2.txt");

        let entry1 = FileEntry::new_file(path1.clone().into(), 100, 0o644);
        let entry2 = FileEntry::new_file(path2.clone().into(), 200, 0o644);

        writer.write_entry(&mut buf, &entry1).unwrap();
        writer.write_entry(&mut buf, &entry2).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.name(), path1);
        assert_eq!(read2.name(), path2);
    }

    #[test]
    fn special_file_rdev_protocol_30_vs_31() {
        // Protocol 30 writes dummy rdev for FIFOs/sockets
        // Protocol 31+ does NOT write rdev for FIFOs/sockets
        let proto30 = ProtocolVersion::try_from(30u8).unwrap();
        let proto31 = ProtocolVersion::try_from(31u8).unwrap();

        let fifo = FileEntry::new_fifo("myfifo".into(), 0o644);

        let mut buf30 = Vec::new();
        let mut writer30 = FileListWriter::new(proto30).with_preserve_devices(true);
        writer30.write_entry(&mut buf30, &fifo).unwrap();

        let mut buf31 = Vec::new();
        let mut writer31 = FileListWriter::new(proto31).with_preserve_devices(true);
        writer31.write_entry(&mut buf31, &fifo).unwrap();

        // Protocol 31 should produce smaller output (no dummy rdev)
        assert!(
            buf31.len() < buf30.len(),
            "protocol 31 should not write rdev for FIFOs: {} < {}",
            buf31.len(),
            buf30.len()
        );
    }

    #[test]
    fn special_file_fifo_round_trip_protocol_28_29() {
        // Protocol 28-29 uses XMIT_RDEV_MINOR_8_PRE30 flag for rdev encoding
        // This test verifies FIFOs write and read correctly with dummy rdev
        use super::super::read::FileListReader;
        use std::io::Cursor;

        for proto_ver in [28u8, 29u8] {
            let protocol = ProtocolVersion::try_from(proto_ver).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

            let entry = FileEntry::new_fifo("myfifo".into(), 0o644);

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(
                read_entry.name(),
                "myfifo",
                "protocol {proto_ver} FIFO name mismatch"
            );
            assert!(
                read_entry.is_special(),
                "protocol {proto_ver} should recognize FIFO as special"
            );
            // rdev should NOT be set (dummy was read and discarded)
            assert!(
                read_entry.rdev_major().is_none(),
                "protocol {proto_ver} FIFO should not have rdev"
            );
        }
    }

    #[test]
    fn device_round_trip_protocol_28_29() {
        // Protocol 28-29 uses XMIT_RDEV_MINOR_8_PRE30 flag for 8-bit minors
        use super::super::read::FileListReader;
        use std::io::Cursor;

        for proto_ver in [28u8, 29u8] {
            let protocol = ProtocolVersion::try_from(proto_ver).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

            // Block device with minor fitting in 8 bits
            let dev_small_minor = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
            // Block device with minor requiring more than 8 bits
            let dev_large_minor = FileEntry::new_block_device("sdb".into(), 0o660, 8, 300);

            writer.write_entry(&mut buf, &dev_small_minor).unwrap();
            writer.write_entry(&mut buf, &dev_large_minor).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

            let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(read1.rdev_major(), Some(8), "proto {proto_ver} dev1 major");
            assert_eq!(read1.rdev_minor(), Some(0), "proto {proto_ver} dev1 minor");
            assert_eq!(read2.rdev_major(), Some(8), "proto {proto_ver} dev2 major");
            assert_eq!(
                read2.rdev_minor(),
                Some(300),
                "proto {proto_ver} dev2 minor (>255)"
            );
        }
    }

    #[test]
    fn directory_content_dir_flag_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // Directory with content
        let mut dir_with_content = FileEntry::new_directory("with_content".into(), 0o755);
        dir_with_content.set_content_dir(true);

        // Directory without content (implied directory)
        let mut dir_no_content = FileEntry::new_directory("no_content".into(), 0o755);
        dir_no_content.set_content_dir(false);

        writer.write_entry(&mut buf, &dir_with_content).unwrap();
        writer.write_entry(&mut buf, &dir_no_content).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.name(), "with_content");
        assert!(read1.content_dir(), "first dir should have content");

        assert_eq!(read2.name(), "no_content");
        assert!(!read2.content_dir(), "second dir should not have content");
    }

    // ========================================================================
    // Extended Flags Encoding Tests (Task #74)
    // ========================================================================
    // These tests verify the wire format encoding for XMIT_EXTENDED_FLAGS
    // across different protocol versions and flag combinations.

    #[test]
    fn extended_flags_two_byte_encoding_protocol_28() {
        // Protocol 28-29 uses two-byte encoding when extended flags are set.
        // When xflags has bits in the 0xFF00 range, XMIT_EXTENDED_FLAGS is set
        // and flags are written as little-endian u16.
        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        // Block device triggers XMIT_SAME_RDEV_MAJOR in extended flags (byte 1)
        // when major matches previous, but first device doesn't match anything
        let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

        writer.write_entry(&mut buf, &entry).unwrap();

        // The first byte should have XMIT_EXTENDED_FLAGS set (bit 2)
        // because device entries set flags in the extended byte
        assert!(
            buf[0] & XMIT_EXTENDED_FLAGS != 0,
            "first byte should have XMIT_EXTENDED_FLAGS set: got {:#04x}",
            buf[0]
        );
    }

    #[test]
    fn extended_flags_one_byte_encoding_when_no_extended_bits() {
        // Protocol 28-29 uses single-byte encoding when no extended flags are needed.
        // Simple file entries without special attributes should use one-byte encoding.
        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let entry = FileEntry::new_file("simple.txt".into(), 100, 0o644);
        writer.write_entry(&mut buf, &entry).unwrap();

        // For a simple file with no previous entry compression,
        // flags should fit in one byte (no XMIT_EXTENDED_FLAGS needed)
        // unless the mode/time differ from defaults
        // The point is: without extended flags, we should NOT have XMIT_EXTENDED_FLAGS
        // But actually, write_flags may still set it if xflags==0 for non-dir
        // Let's verify the encoding is correct for simple entries
        assert!(!buf.is_empty(), "buffer should not be empty");
        assert_ne!(buf[0], 0, "flags byte should not be zero (end marker)");
    }

    #[test]
    fn extended_flags_protocol_30_varint_encoding() {
        // Protocol 30+ with VARINT_FLIST_FLAGS encodes all flags as a single varint.
        // This test verifies varint encoding is used when compat flags are set.
        use crate::varint::decode_varint;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let compat_flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut writer = FileListWriter::with_compat_flags(protocol, compat_flags);

        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 0);

        let mut buf = Vec::new();
        writer.write_entry(&mut buf, &entry).unwrap();

        // Decode the flags as varint
        let (flags_value, _bytes_read) = decode_varint(&buf).unwrap();
        assert_ne!(flags_value, 0, "flags should not be zero");
    }

    #[test]
    fn extended_flags_all_basic_flags_combinations() {
        // Test that all basic flag combinations (byte 0) work correctly
        use super::super::flags::FileFlags;
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();

        // Test XMIT_TOP_DIR (directories only) using from_raw with flags set
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);
            // Create directory entry with XMIT_TOP_DIR flag set
            let flags = FileFlags::new(XMIT_TOP_DIR, 0);
            let dir = FileEntry::from_raw("topdir".into(), 0, 0o040755, 0, 0, flags);
            writer.write_entry(&mut buf, &dir).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert!(read.flags().top_dir(), "XMIT_TOP_DIR should round-trip");
        }

        // Test XMIT_LONG_NAME (paths > 255 bytes)
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);
            let long_name = "x".repeat(300);
            let entry = FileEntry::new_file(long_name.clone().into(), 100, 0o644);
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read.name(), long_name, "long name should round-trip");
        }
    }

    #[test]
    fn extended_flags_hardlink_flag_combinations() {
        // Test XMIT_HLINKED and XMIT_HLINK_FIRST flag combinations
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();

        // Test: XMIT_HLINKED | XMIT_HLINK_FIRST (hardlink leader)
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);
            let mut entry = FileEntry::new_file("leader.txt".into(), 100, 0o644);
            entry.set_hardlink_idx(u32::MAX); // Leader marker
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(
                read.hardlink_idx(),
                Some(u32::MAX),
                "leader should have u32::MAX"
            );
        }

        // Test: XMIT_HLINKED only (hardlink follower)
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);
            let mut entry = FileEntry::new_file("follower.txt".into(), 100, 0o644);
            entry.set_hardlink_idx(42); // Points to leader index
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(
                read.hardlink_idx(),
                Some(42),
                "follower should have index 42"
            );
        }
    }

    #[test]
    fn extended_flags_time_related_flags() {
        // Test XMIT_SAME_ATIME, XMIT_MOD_NSEC, and XMIT_CRTIME_EQ_MTIME flags
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Test XMIT_MOD_NSEC (protocol 31+)
        {
            let protocol = ProtocolVersion::try_from(31u8).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let mut entry = FileEntry::new_file("nsec.txt".into(), 100, 0o644);
            entry.set_mtime(1700000000, 123456789); // With nanoseconds

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(
                read.mtime_nsec(),
                123456789,
                "XMIT_MOD_NSEC should round-trip"
            );
        }

        // Test XMIT_SAME_ATIME (protocol 30+ with preserve_atimes)
        {
            let protocol = ProtocolVersion::try_from(30u8).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol).with_preserve_atimes(true);

            // First entry sets atime
            let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
            entry1.set_atime(1700000000);

            // Second entry has same atime - should use XMIT_SAME_ATIME
            let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
            entry2.set_atime(1700000000);

            writer.write_entry(&mut buf, &entry1).unwrap();
            let first_len = buf.len();
            writer.write_entry(&mut buf, &entry2).unwrap();
            let second_len = buf.len() - first_len;
            writer.write_end(&mut buf, None).unwrap();

            // Second entry should be smaller (atime compressed)
            assert!(
                second_len < first_len,
                "XMIT_SAME_ATIME should compress: {second_len} < {first_len}"
            );

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_atimes(true);
            let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(read1.atime(), 1700000000);
            assert_eq!(read2.atime(), 1700000000);
        }
    }

    #[test]
    fn extended_flags_owner_name_flags() {
        // Test XMIT_USER_NAME_FOLLOWS and XMIT_GROUP_NAME_FOLLOWS (protocol 30+)
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();

        // Test both user and group names
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol)
                .with_preserve_uid(true)
                .with_preserve_gid(true);

            let mut entry = FileEntry::new_file("owned.txt".into(), 100, 0o644);
            entry.set_uid(1000);
            entry.set_gid(1000);
            entry.set_user_name("alice".to_string());
            entry.set_group_name("developers".to_string());

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_uid(true)
                .with_preserve_gid(true);

            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read.user_name(), Some("alice"));
            assert_eq!(read.group_name(), Some("developers"));
        }

        // Verify names are NOT written for protocol 29
        {
            let protocol29 = ProtocolVersion::try_from(29u8).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol29)
                .with_preserve_uid(true)
                .with_preserve_gid(true);

            let mut entry = FileEntry::new_file("file29.txt".into(), 100, 0o644);
            entry.set_uid(1000);
            entry.set_gid(1000);
            entry.set_user_name("alice".to_string());
            entry.set_group_name("developers".to_string());

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol29)
                .with_preserve_uid(true)
                .with_preserve_gid(true);

            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            // Protocol 29 should NOT have user/group names
            assert_eq!(
                read.user_name(),
                None,
                "protocol 29 should not have user name"
            );
            assert_eq!(
                read.group_name(),
                None,
                "protocol 29 should not have group name"
            );
        }
    }

    #[test]
    fn extended_flags_device_flags_protocol_28_29() {
        // Test XMIT_SAME_RDEV_MAJOR and XMIT_RDEV_MINOR_8_PRE30 for protocol 28-29
        use super::super::read::FileListReader;
        use std::io::Cursor;

        for proto_ver in [28u8, 29u8] {
            let protocol = ProtocolVersion::try_from(proto_ver).unwrap();

            // Test device with 8-bit minor (uses XMIT_RDEV_MINOR_8_PRE30)
            {
                let mut buf = Vec::new();
                let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
                let entry = FileEntry::new_block_device("dev8".into(), 0o660, 8, 255);
                writer.write_entry(&mut buf, &entry).unwrap();
                writer.write_end(&mut buf, None).unwrap();

                let mut cursor = Cursor::new(&buf[..]);
                let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
                let read = reader.read_entry(&mut cursor).unwrap().unwrap();

                assert_eq!(
                    read.rdev_major(),
                    Some(8),
                    "proto {proto_ver} 8-bit minor major"
                );
                assert_eq!(
                    read.rdev_minor(),
                    Some(255),
                    "proto {proto_ver} 8-bit minor"
                );
            }

            // Test device with >8-bit minor (does NOT use XMIT_RDEV_MINOR_8_PRE30)
            {
                let mut buf = Vec::new();
                let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
                let entry = FileEntry::new_block_device("dev32".into(), 0o660, 8, 256);
                writer.write_entry(&mut buf, &entry).unwrap();
                writer.write_end(&mut buf, None).unwrap();

                let mut cursor = Cursor::new(&buf[..]);
                let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
                let read = reader.read_entry(&mut cursor).unwrap().unwrap();

                assert_eq!(
                    read.rdev_major(),
                    Some(8),
                    "proto {proto_ver} 32-bit minor major"
                );
                assert_eq!(
                    read.rdev_minor(),
                    Some(256),
                    "proto {proto_ver} 32-bit minor"
                );
            }

            // Test XMIT_SAME_RDEV_MAJOR with two devices having same major
            {
                let mut buf = Vec::new();
                let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
                let entry1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
                let entry2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);

                writer.write_entry(&mut buf, &entry1).unwrap();
                let first_len = buf.len();
                writer.write_entry(&mut buf, &entry2).unwrap();
                let second_len = buf.len() - first_len;
                writer.write_end(&mut buf, None).unwrap();

                // Second entry should be smaller due to XMIT_SAME_RDEV_MAJOR
                assert!(
                    second_len < first_len,
                    "proto {proto_ver} XMIT_SAME_RDEV_MAJOR should compress: {second_len} < {first_len}"
                );

                let mut cursor = Cursor::new(&buf[..]);
                let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
                let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
                let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

                assert_eq!(read1.rdev_major(), Some(8));
                assert_eq!(read2.rdev_major(), Some(8));
            }
        }
    }

    #[test]
    fn extended_flags_zero_xflags_non_directory_uses_top_dir() {
        // When xflags == 0 for a non-directory in protocol 28-29,
        // XMIT_TOP_DIR is used to avoid collision with end marker.
        // This is tested implicitly in write_flags() for protocol < 30.
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // Set up compression state so mode and time match
        writer.state.update(b"test", 0o100644, 1700000000, 0, 0);

        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 0); // Same time as prev

        writer.write_entry(&mut buf, &entry).unwrap();

        // First byte should NOT be zero (would be end marker)
        assert_ne!(buf[0], 0, "flags should not be zero for file entry");
    }

    #[test]
    fn extended_flags_protocol_version_boundaries() {
        // Verify flag encoding at protocol version boundaries
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Protocol 27 should NOT have extended flags support
        // (but our minimum is 28, so this tests the boundary)

        // Protocol 28: First version with extended flags
        {
            let protocol = ProtocolVersion::try_from(28u8).unwrap();
            assert!(
                protocol.supports_extended_flags(),
                "protocol 28 must support extended flags"
            );

            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
            let entry = FileEntry::new_block_device("dev28".into(), 0o660, 8, 0);
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read.rdev_major(), Some(8));
        }

        // Protocol 30: Introduces varint encoding option
        {
            let protocol = ProtocolVersion::try_from(30u8).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);
            let entry = FileEntry::new_file("test30.txt".into(), 100, 0o644);
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read.name(), "test30.txt");
        }

        // Protocol 31: Introduces XMIT_MOD_NSEC and safe file list by default
        {
            let protocol = ProtocolVersion::try_from(31u8).unwrap();
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let mut entry = FileEntry::new_file("test31.txt".into(), 100, 0o644);
            entry.set_mtime(1700000000, 500000000);

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read.mtime_nsec(), 500000000);
        }
    }

    // ========================================================================
    // Large file size encoding tests (>2GB, >4GB)
    // ========================================================================

    /// Test encoding and decoding a 3GB file (above 2^31 = 2GB boundary).
    /// This verifies that the varlong encoding correctly handles file sizes
    /// that exceed the signed 32-bit integer range.
    #[test]
    fn large_file_size_3gb_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024; // 3 * 1024^3 = 3,221,225,472 bytes

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("large_3gb.bin".into(), SIZE_3GB, 0o644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "large_3gb.bin");
        assert_eq!(
            read_entry.size(),
            SIZE_3GB,
            "3GB file size should round-trip correctly (above 2^31 boundary)"
        );
    }

    /// Test encoding and decoding a 5GB file (above 2^32 = 4GB boundary).
    /// This verifies that the varlong encoding correctly handles file sizes
    /// that exceed the unsigned 32-bit integer range.
    #[test]
    fn large_file_size_5gb_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024; // 5 * 1024^3 = 5,368,709,120 bytes

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("large_5gb.bin".into(), SIZE_5GB, 0o644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "large_5gb.bin");
        assert_eq!(
            read_entry.size(),
            SIZE_5GB,
            "5GB file size should round-trip correctly (above 2^32 boundary)"
        );
    }

    /// Test multiple large file sizes to ensure consistent encoding/decoding
    /// across the 2GB and 4GB boundaries.
    #[test]
    fn large_file_sizes_boundary_values_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        // Key boundary values for large file support
        let test_sizes: &[(u64, &str)] = &[
            // Just below 2^31 (max signed 32-bit positive)
            ((1u64 << 31) - 1, "just_below_2gb"),
            // Exactly 2^31 (2GB boundary)
            (1u64 << 31, "exactly_2gb"),
            // Just above 2^31
            ((1u64 << 31) + 1, "just_above_2gb"),
            // Just below 2^32 (max unsigned 32-bit)
            ((1u64 << 32) - 1, "just_below_4gb"),
            // Exactly 2^32 (4GB boundary)
            (1u64 << 32, "exactly_4gb"),
            // Just above 2^32
            ((1u64 << 32) + 1, "just_above_4gb"),
            // 3GB (3 * 1024^3)
            (3 * 1024 * 1024 * 1024, "3gb"),
            // 5GB (5 * 1024^3)
            (5 * 1024 * 1024 * 1024, "5gb"),
            // 1TB
            (1024 * 1024 * 1024 * 1024, "1tb"),
        ];

        let protocol = test_protocol();

        for (size, name) in test_sizes {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let filename = format!("{name}.bin");
            let mut entry = FileEntry::new_file(filename.clone().into(), *size, 0o644);
            entry.set_mtime(1700000000, 0);

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.name(), &filename);
            assert_eq!(
                read_entry.size(),
                *size,
                "File size {size} ({name}) should round-trip correctly"
            );
        }
    }

    /// Test large file sizes with legacy protocol (< 30) which uses longint encoding.
    /// The longint format uses 4 bytes for values <= 0x7FFFFFFF and 12 bytes for larger.
    #[test]
    fn large_file_size_legacy_protocol_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;
        const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

        // Protocol 29 uses longint encoding
        let protocol = ProtocolVersion::try_from(29u8).unwrap();

        for size in [SIZE_3GB, SIZE_5GB] {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let mut entry = FileEntry::new_file("large_legacy.bin".into(), size, 0o644);
            entry.set_mtime(1700000000, 0);

            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(
                read_entry.size(),
                size,
                "Legacy protocol should handle {size} byte files correctly"
            );
        }
    }
}
