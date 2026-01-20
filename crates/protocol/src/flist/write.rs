//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver.

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
    #[must_use]
    pub const fn with_preserve_uid(mut self, preserve: bool) -> Self {
        self.preserve_uid = preserve;
        self
    }

    /// Sets whether GID values should be written to the wire.
    #[must_use]
    pub const fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.preserve_gid = preserve;
        self
    }

    /// Sets whether symlink targets should be written to the wire.
    #[must_use]
    pub const fn with_preserve_links(mut self, preserve: bool) -> Self {
        self.preserve_links = preserve;
        self
    }

    /// Sets whether device numbers should be written to the wire.
    #[must_use]
    pub const fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Sets whether hardlink indices should be written to the wire.
    #[must_use]
    pub const fn with_preserve_hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Sets whether access times should be written to the wire.
    #[must_use]
    pub const fn with_preserve_atimes(mut self, preserve: bool) -> Self {
        self.preserve_atimes = preserve;
        self
    }

    /// Sets whether creation times should be written to the wire.
    #[must_use]
    pub const fn with_preserve_crtimes(mut self, preserve: bool) -> Self {
        self.preserve_crtimes = preserve;
        self
    }

    /// Sets whether ACLs should be written to the wire.
    ///
    /// When enabled, ACL indices are written after other metadata.
    /// Note: ACL data itself is sent in a separate exchange.
    #[must_use]
    pub const fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Sets whether extended attributes should be written to the wire.
    ///
    /// When enabled, xattr indices are written after ACL indices.
    /// Note: Xattr data itself is sent in a separate exchange.
    #[must_use]
    pub const fn with_preserve_xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Enables checksum mode (--checksum / -c) with the given checksum length.
    ///
    /// When enabled, checksums are written for regular files. For protocol < 28,
    /// checksums are also written for non-regular files (using empty_sum).
    #[must_use]
    pub const fn with_always_checksum(mut self, csum_len: usize) -> Self {
        self.always_checksum = true;
        self.flist_csum_len = csum_len;
        self
    }

    /// Sets the filename encoding converter for iconv support.
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

        // Device/special file rdev major comparison (protocol 28+)
        // Devices always, special files only for protocol < 31
        let needs_rdev = self.preserve_devices
            && (entry.is_device() || (entry.is_special() && self.protocol.as_u8() < 31));

        if needs_rdev {
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
        }

        // Hardlink flags (protocol 28+, non-directories only)
        if self.preserve_hard_links && !entry.is_dir() {
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
        }

        // User name follows flag (protocol 30+)
        if self.preserve_uid
            && self.protocol.as_u8() >= 30
            && entry.user_name().is_some()
            && (xflags & (XMIT_SAME_UID as u32)) == 0
        {
            xflags |= (XMIT_USER_NAME_FOLLOWS as u32) << 8;
        }

        // Group name follows flag (protocol 30+)
        if self.preserve_gid
            && self.protocol.as_u8() >= 30
            && entry.group_name().is_some()
            && (xflags & (XMIT_SAME_GID as u32)) == 0
        {
            xflags |= (XMIT_GROUP_NAME_FOLLOWS as u32) << 8;
        }

        // No content directory flag (protocol 30+, directories only)
        // Note: shares bit position with XMIT_SAME_RDEV_MAJOR (devices vs dirs)
        if entry.is_dir() && self.protocol.as_u8() >= 30 && !entry.content_dir() {
            xflags |= (XMIT_NO_CONTENT_DIR as u32) << 8;
        }

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

    /// Writes flags to the wire in the appropriate format.
    fn write_flags<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        xflags: u32,
        is_dir: bool,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: avoid xflags=0 which looks like end marker
            let flags_to_write = if xflags == 0 {
                XMIT_LONG_NAME as u32
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
        // 1. Write file size
        self.codec.write_file_size(writer, entry.size() as i64)?;

        // 2. Write mtime if different
        if xflags & (XMIT_SAME_TIME as u32) == 0 {
            self.codec.write_mtime(writer, entry.mtime())?;
        }

        // 3. Write nsec if flag set (protocol 31+)
        if (xflags & ((XMIT_MOD_NSEC as u32) << 8)) != 0 {
            write_varint(writer, entry.mtime_nsec() as i32)?;
        }

        // 4. Write crtime if preserving and different from mtime
        if self.preserve_crtimes && (xflags & ((XMIT_CRTIME_EQ_MTIME as u32) << 16)) == 0 {
            crate::write_varlong(writer, entry.crtime(), 4)?;
        }

        // 5. Write mode if different
        if xflags & (XMIT_SAME_MODE as u32) == 0 {
            let wire_mode = entry.mode() as i32;
            writer.write_all(&wire_mode.to_le_bytes())?;
        }

        // 6. Write atime if preserving and different (non-directories only)
        if self.preserve_atimes
            && !entry.is_dir()
            && (xflags & ((XMIT_SAME_ATIME as u32) << 8)) == 0
        {
            crate::write_varlong(writer, entry.atime(), 4)?;
            self.state.update_atime(entry.atime());
        }

        // 7. Write UID if preserving and different
        let entry_uid = entry.uid().unwrap_or(0);
        if self.preserve_uid && (xflags & (XMIT_SAME_UID as u32)) == 0 {
            if self.protocol.uses_fixed_encoding() {
                writer.write_all(&(entry_uid as i32).to_le_bytes())?;
            } else {
                write_varint(writer, entry_uid as i32)?;
                // User name follows UID (protocol 30+)
                if (xflags & ((XMIT_USER_NAME_FOLLOWS as u32) << 8)) != 0 {
                    if let Some(name) = entry.user_name() {
                        let name_bytes = name.as_bytes();
                        let len = name_bytes.len().min(255) as u8;
                        writer.write_all(&[len])?;
                        writer.write_all(&name_bytes[..len as usize])?;
                    }
                }
            }
            self.state.update_uid(entry_uid);
        }

        // 8. Write GID if preserving and different
        let entry_gid = entry.gid().unwrap_or(0);
        if self.preserve_gid && (xflags & (XMIT_SAME_GID as u32)) == 0 {
            if self.protocol.uses_fixed_encoding() {
                writer.write_all(&(entry_gid as i32).to_le_bytes())?;
            } else {
                write_varint(writer, entry_gid as i32)?;
                // Group name follows GID (protocol 30+)
                if (xflags & ((XMIT_GROUP_NAME_FOLLOWS as u32) << 8)) != 0 {
                    if let Some(name) = entry.group_name() {
                        let name_bytes = name.as_bytes();
                        let len = name_bytes.len().min(255) as u8;
                        writer.write_all(&[len])?;
                        writer.write_all(&name_bytes[..len as usize])?;
                    }
                }
            }
            self.state.update_gid(entry_gid);
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
        // Step 1: Apply encoding conversion
        let raw_name = entry.name().as_bytes();
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
}
