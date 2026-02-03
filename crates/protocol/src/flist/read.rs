//! File list reading (decoding) from the rsync wire format.
//!
//! This module implements the receiver side of file list exchange, decoding
//! file entries as they arrive from the sender. The reader maintains compression
//! state to handle fields that are omitted when they match the previous entry.
//!
//! # Upstream Reference
//!
//! See `flist.c:recv_file_entry()` for the canonical wire format decoding.

use std::io::{self, Read};
use std::path::PathBuf;

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::codec::{ProtocolCodec, ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::varint::{read_varint, read_varint30_int};

use super::entry::FileEntry;
use super::flags::{
    FileFlags, XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST,
    XMIT_NO_CONTENT_DIR,
};
use super::state::{FileListCompressionState, FileListStats};

/// Result of reading flags from the wire.
#[derive(Debug)]
enum FlagsResult {
    /// End of file list reached (zero flags byte).
    EndOfList,
    /// I/O error marker with error code from sender.
    IoError(i32),
    /// Valid flags for a file entry.
    Flags(FileFlags),
}

/// State maintained while reading a file list.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This reader maintains the necessary state
/// to decode these compressed entries.
#[derive(Debug)]
pub struct FileListReader {
    /// Protocol version being used.
    protocol: ProtocolVersion,
    /// Protocol codec for version-aware encoding/decoding.
    codec: ProtocolCodecEnum,
    /// Compatibility flags for this session.
    compat_flags: Option<CompatibilityFlags>,
    /// Compression state for cross-entry field sharing.
    state: FileListCompressionState,
    /// Statistics collected during file list reading.
    stats: FileListStats,
    /// Whether to preserve (and thus read) UID values from the wire.
    preserve_uid: bool,
    /// Whether to preserve (and thus read) GID values from the wire.
    preserve_gid: bool,
    /// Whether to preserve (and thus read) symlink targets from the wire.
    preserve_links: bool,
    /// Whether to preserve (and thus read) device numbers from the wire.
    preserve_devices: bool,
    /// Whether to preserve (and thus read) hardlink indices from the wire.
    preserve_hard_links: bool,
    /// Whether to preserve (and thus read) access times from the wire.
    preserve_atimes: bool,
    /// Whether to preserve (and thus read) creation times from the wire.
    preserve_crtimes: bool,
    /// Whether sender is in checksum mode (--checksum / -c).
    always_checksum: bool,
    /// Whether to preserve (and thus read) ACLs from the wire.
    preserve_acls: bool,
    /// Whether to preserve (and thus read) extended attributes from the wire.
    preserve_xattrs: bool,
    /// Length of checksum to read (depends on protocol and checksum algorithm).
    flist_csum_len: usize,
    /// Optional filename encoding converter (for --iconv support).
    iconv: Option<FilenameConverter>,
}

/// Result from reading metadata fields.
///
/// Contains all metadata decoded from the wire format for a single file entry.
/// Fields are `Option` when they may be conditionally present based on
/// protocol options (preserve_uid, preserve_gid, preserve_atimes, etc.).
struct MetadataResult {
    /// Modification time in seconds since Unix epoch.
    mtime: i64,
    /// Nanosecond component of modification time (protocol 31+).
    nsec: u32,
    /// Unix mode bits (file type and permissions).
    mode: u32,
    /// User ID (when preserve_uid is enabled).
    uid: Option<u32>,
    /// Group ID (when preserve_gid is enabled).
    gid: Option<u32>,
    /// User name for UID mapping (protocol 30+).
    user_name: Option<String>,
    /// Group name for GID mapping (protocol 30+).
    group_name: Option<String>,
    /// Access time (when preserve_atimes is enabled, non-directories only).
    atime: Option<i64>,
    /// Creation time (when preserve_crtimes is enabled).
    crtime: Option<i64>,
    /// Whether directory has content to transfer (protocol 30+, directories only).
    content_dir: bool,
}

impl FileListReader {
    /// Creates a new file list reader for the given protocol version.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        let codec = create_protocol_codec(protocol.as_u8());

        Self {
            protocol,
            codec,
            compat_flags: None,
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
        }
    }

    /// Creates a new file list reader with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        let codec = create_protocol_codec(protocol.as_u8());

        Self {
            protocol,
            codec,
            compat_flags: Some(compat_flags),
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
        }
    }

    /// Sets whether UID values should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_uid(mut self, preserve: bool) -> Self {
        self.preserve_uid = preserve;
        self
    }

    /// Sets whether GID values should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.preserve_gid = preserve;
        self
    }

    /// Sets whether symlink targets should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_links(mut self, preserve: bool) -> Self {
        self.preserve_links = preserve;
        self
    }

    /// Sets whether device numbers should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Sets whether hardlink indices should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Sets whether access times should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_atimes(mut self, preserve: bool) -> Self {
        self.preserve_atimes = preserve;
        self
    }

    /// Sets whether creation times should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_crtimes(mut self, preserve: bool) -> Self {
        self.preserve_crtimes = preserve;
        self
    }

    /// Sets whether ACLs should be read from the wire.
    ///
    /// When enabled, ACL indices are read after other metadata.
    /// Note: ACL data itself is received in a separate exchange.
    #[inline]
    #[must_use]
    pub const fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Sets whether extended attributes should be read from the wire.
    ///
    /// When enabled, xattr indices are read after ACL indices.
    /// Note: Xattr data itself is received in a separate exchange.
    #[inline]
    #[must_use]
    pub const fn with_preserve_xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Enables checksum mode (--checksum / -c) with the given checksum length.
    ///
    /// When enabled, checksums are read for regular files. For protocol < 28,
    /// checksums are also read for non-regular files (empty_sum).
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

    /// Returns the statistics collected during file list reading.
    #[must_use]
    pub const fn stats(&self) -> &FileListStats {
        &self.stats
    }

    /// Returns whether varint flag encoding is enabled.
    #[inline]
    fn use_varint_flags(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::VARINT_FLIST_FLAGS))
    }

    /// Returns whether safe file list mode is enabled.
    #[inline]
    fn use_safe_file_list(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::SAFE_FILE_LIST))
            || self.protocol.safe_file_list_always_enabled()
    }

    /// Reads and validates flags from the wire.
    ///
    /// Returns `FlagsResult::EndOfList` for end-of-list marker,
    /// `FlagsResult::IoError` for I/O error markers, or
    /// `FlagsResult::Flags` for valid entry flags.
    fn read_flags<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<FlagsResult> {
        let use_varint = self.use_varint_flags();

        // Read primary flags
        let flags_value = if use_varint {
            read_varint(reader)?
        } else {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0] as i32
        };

        // Check for end-of-list marker
        if flags_value == 0 {
            if use_varint {
                // In varint mode, error code follows zero flags
                let io_error = read_varint(reader)?;
                if io_error != 0 {
                    return Ok(FlagsResult::IoError(io_error));
                }
            }
            return Ok(FlagsResult::EndOfList);
        }

        // Read extended flags
        let (ext_byte, ext16_byte) = if use_varint {
            (
                ((flags_value >> 8) & 0xFF) as u8,
                ((flags_value >> 16) & 0xFF) as u8,
            )
        } else if (flags_value as u8 & XMIT_EXTENDED_FLAGS) != 0 {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            (buf[0], 0u8)
        } else {
            (0u8, 0u8)
        };

        let primary_byte = flags_value as u8;

        // Check for I/O error marker
        if let Some(error) = self.check_error_marker(primary_byte, ext_byte, reader)? {
            return Ok(FlagsResult::IoError(error));
        }

        // Build flags structure
        let flags = if ext_byte != 0 || ext16_byte != 0 || (primary_byte & XMIT_EXTENDED_FLAGS) != 0
        {
            FileFlags::new_with_extended16(primary_byte, ext_byte, ext16_byte)
        } else {
            FileFlags::new(primary_byte, 0)
        };

        Ok(FlagsResult::Flags(flags))
    }

    /// Checks for I/O error marker in flags.
    ///
    /// Returns `Some(error_code)` if an error marker is detected,
    /// `None` if flags represent a valid entry.
    fn check_error_marker<R: Read + ?Sized>(
        &self,
        primary: u8,
        extended: u8,
        reader: &mut R,
    ) -> io::Result<Option<i32>> {
        let flags_value = (primary as i32) | ((extended as i32) << 8);
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

        if flags_value != error_marker {
            return Ok(None);
        }

        if !self.use_safe_file_list() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid flist flag: {flags_value:#x}"),
            ));
        }

        let error_code = read_varint(reader)?;
        Ok(Some(error_code))
    }

    /// Reads metadata fields in upstream rsync wire format order.
    ///
    /// This function decodes the variable-length metadata section of a file entry.
    /// Fields are conditionally present based on XMIT flags - when a "SAME" flag is
    /// set, the field is omitted and the previous entry's value is reused.
    ///
    /// # Wire Format Order
    ///
    /// Fields are read in this exact order (matching flist.c recv_file_entry lines 826-920):
    ///
    /// | Order | Field | Condition | Encoding |
    /// |-------|-------|-----------|----------|
    /// | 1 | mtime | `!XMIT_SAME_TIME` | varlong(4) |
    /// | 2 | nsec | `XMIT_MOD_NSEC` (proto 31+) | varint30 |
    /// | 3 | crtime | `preserve_crtimes && !XMIT_CRTIME_EQ_MTIME` | varlong(4) |
    /// | 4 | mode | `!XMIT_SAME_MODE` | i32 LE (proto <30) or varint |
    /// | 5 | atime | `preserve_atimes && !is_dir && !XMIT_SAME_ATIME` | varlong(4) |
    /// | 6 | uid | `preserve_uid && !XMIT_SAME_UID` | i32 LE (proto <30) or varint |
    /// | 6a | user_name | `XMIT_USER_NAME_FOLLOWS` (proto 30+) | u8 len + bytes |
    /// | 7 | gid | `preserve_gid && !XMIT_SAME_GID` | i32 LE (proto <30) or varint |
    /// | 7a | group_name | `XMIT_GROUP_NAME_FOLLOWS` (proto 30+) | u8 len + bytes |
    ///
    /// # Arguments
    ///
    /// * `reader` - The byte stream to read from
    /// * `flags` - The XMIT flags indicating which fields are present
    ///
    /// # Returns
    ///
    /// A `MetadataResult` containing all decoded metadata fields.
    ///
    /// # Upstream Reference
    ///
    /// See `flist.c:recv_file_entry()` lines 826-920 for the metadata reading logic.
    fn read_metadata<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<MetadataResult> {
        // 1. Read mtime
        let mtime = if flags.same_time() {
            self.state.prev_mtime()
        } else {
            let mtime = crate::read_varlong(reader, 4)?;
            self.state.update_mtime(mtime);
            mtime
        };

        // 2. Read nanoseconds if flag set (protocol 31+)
        let nsec = if flags.mod_nsec() {
            crate::read_varint(reader)? as u32
        } else {
            0
        };

        // 3. Read crtime if preserving crtimes (BEFORE mode, per upstream)
        let crtime = if self.preserve_crtimes {
            if flags.crtime_eq_mtime() {
                // Creation time equals mtime
                Some(mtime)
            } else {
                // Read crtime from wire
                let crtime = crate::read_varlong(reader, 4)?;
                Some(crtime)
            }
        } else {
            None
        };

        // 4. Read mode
        let mode = if flags.same_mode() {
            self.state.prev_mode()
        } else {
            let mut mode_bytes = [0u8; 4];
            reader.read_exact(&mut mode_bytes)?;
            let mode = i32::from_le_bytes(mode_bytes) as u32;
            self.state.update_mode(mode);
            mode
        };

        // Determine if this is a directory (needed for atime and content_dir)
        let is_dir = (mode & 0o170000) == 0o040000;

        // 5. Read atime if preserving atimes (AFTER mode, non-directories only)
        let atime = if self.preserve_atimes && !is_dir {
            if flags.same_atime() {
                Some(self.state.prev_atime())
            } else {
                let atime = crate::read_varlong(reader, 4)?;
                self.state.update_atime(atime);
                Some(atime)
            }
        } else {
            None
        };

        // 6. Read UID and optional user name
        let mut user_name = None;
        let uid = if self.preserve_uid {
            if flags.same_uid() {
                // Use previous UID
                Some(self.state.prev_uid())
            } else {
                // Read UID from wire (fixed encoding for protocol < 30)
                let uid = if self.protocol.uses_fixed_encoding() {
                    let mut buf = [0u8; 4];
                    reader.read_exact(&mut buf)?;
                    i32::from_le_bytes(buf) as u32
                } else {
                    read_varint(reader)? as u32
                };
                self.state.update_uid(uid);
                // Read user name if flag set (protocol 30+)
                if flags.user_name_follows() {
                    let mut len_buf = [0u8; 1];
                    reader.read_exact(&mut len_buf)?;
                    let len = len_buf[0] as usize;
                    if len > 0 {
                        let mut name_bytes = vec![0u8; len];
                        reader.read_exact(&mut name_bytes)?;
                        user_name = Some(String::from_utf8_lossy(&name_bytes).into_owned());
                    }
                }
                Some(uid)
            }
        } else {
            None
        };

        // 7. Read GID and optional group name
        let mut group_name = None;
        let gid = if self.preserve_gid {
            if flags.same_gid() {
                // Use previous GID
                Some(self.state.prev_gid())
            } else {
                // Read GID from wire (fixed encoding for protocol < 30)
                let gid = if self.protocol.uses_fixed_encoding() {
                    let mut buf = [0u8; 4];
                    reader.read_exact(&mut buf)?;
                    i32::from_le_bytes(buf) as u32
                } else {
                    read_varint(reader)? as u32
                };
                self.state.update_gid(gid);
                // Read group name if flag set (protocol 30+)
                if flags.group_name_follows() {
                    let mut len_buf = [0u8; 1];
                    reader.read_exact(&mut len_buf)?;
                    let len = len_buf[0] as usize;
                    if len > 0 {
                        let mut name_bytes = vec![0u8; len];
                        reader.read_exact(&mut name_bytes)?;
                        group_name = Some(String::from_utf8_lossy(&name_bytes).into_owned());
                    }
                }
                Some(gid)
            }
        } else {
            None
        };

        // Determine content_dir for directories (protocol 30+)
        // XMIT_NO_CONTENT_DIR shares bit with XMIT_SAME_RDEV_MAJOR but only applies to directories
        let content_dir = if is_dir && self.protocol.as_u8() >= 30 {
            // If XMIT_NO_CONTENT_DIR is NOT set, directory has content
            (flags.extended & XMIT_NO_CONTENT_DIR) == 0
        } else {
            // Non-directories or older protocols: default to true
            true
        };

        Ok(MetadataResult {
            mtime,
            nsec,
            mode,
            uid,
            gid,
            user_name,
            group_name,
            atime,
            crtime,
            content_dir,
        })
    }

    /// Reads symlink target if mode indicates a symlink AND preserve_links is enabled.
    ///
    /// The sender only transmits symlink targets when preserve_links is negotiated.
    /// If preserve_links is false, the sender omits symlink targets, so we must NOT
    /// attempt to read them from the stream.
    ///
    /// Wire format: varint30(len) + raw bytes
    fn read_symlink_target<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        mode: u32,
    ) -> io::Result<Option<PathBuf>> {
        // S_IFLNK check: mode & 0o170000 == 0o120000
        let is_symlink = mode & 0o170000 == 0o120000;

        // Only read symlink target if this is a symlink AND preserve_links is enabled.
        // The sender only sends symlink targets when preserve_links is true.
        if !is_symlink || !self.preserve_links {
            return Ok(None);
        }

        let len = read_varint30_int(reader, self.protocol.as_u8())? as usize;
        if len == 0 {
            return Ok(None);
        }

        let mut target_bytes = vec![0u8; len];
        reader.read_exact(&mut target_bytes)?;

        // Convert bytes to PathBuf (platform-specific handling)
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let target = std::ffi::OsStr::from_bytes(&target_bytes);
            Ok(Some(PathBuf::from(target)))
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, attempt UTF-8 conversion
            let target_str = String::from_utf8_lossy(&target_bytes);
            Ok(Some(PathBuf::from(target_str.into_owned())))
        }
    }

    /// Reads device numbers if preserving devices and mode indicates a device.
    ///
    /// Also reads dummy rdev for special files (FIFOs, sockets) in protocol < 31.
    ///
    /// Wire format (protocol 28+):
    /// - Major: varint30 (omitted if XMIT_SAME_RDEV_MAJOR set)
    /// - Minor: varint (protocol 30+) or byte/int (protocol 28-29)
    fn read_rdev<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        mode: u32,
        flags: FileFlags,
    ) -> io::Result<Option<(u32, u32)>> {
        let type_bits = mode & 0o170000;
        let is_device = type_bits == 0o060000 || type_bits == 0o020000; // S_ISBLK or S_ISCHR
        let is_special = type_bits == 0o140000 || type_bits == 0o010000; // S_IFSOCK or S_IFIFO

        // Devices always, special files only for protocol < 31
        let needs_rdev =
            self.preserve_devices && (is_device || (is_special && self.protocol.as_u8() < 31));

        if !needs_rdev {
            return Ok(None);
        }

        // Read major if not same as previous
        let major = if flags.same_rdev_major() {
            self.state.prev_rdev_major()
        } else {
            let m = read_varint30_int(reader, self.protocol.as_u8())? as u32;
            self.state.update_rdev_major(m);
            m
        };

        // Read minor
        let minor = if self.protocol.as_u8() >= 30 {
            read_varint(reader)? as u32
        } else {
            // Protocol 28-29: read byte or int based on XMIT_RDEV_MINOR_8_pre30
            let minor_is_byte = flags.rdev_minor_8_pre30();
            if minor_is_byte {
                let mut buf = [0u8; 1];
                reader.read_exact(&mut buf)?;
                buf[0] as u32
            } else {
                let mut buf = [0u8; 4];
                reader.read_exact(&mut buf)?;
                i32::from_le_bytes(buf) as u32
            }
        };

        // For special files, we read but don't return the dummy rdev
        if is_special {
            return Ok(None);
        }

        Ok(Some((major, minor)))
    }

    /// Reads hardlink device and inode for protocol 28-29.
    ///
    /// In protocols before 30, hardlinks are identified by (dev, ino) pairs
    /// rather than indices.
    ///
    /// Wire format:
    /// - If not XMIT_SAME_DEV_PRE30: read longint as dev (stored as dev + 1)
    /// - Always read longint as ino
    fn read_hardlink_dev_ino<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
        mode: u32,
    ) -> io::Result<Option<(i64, i64)>> {
        // Only for protocol 28-29, non-directories
        if !self.preserve_hard_links || self.protocol.as_u8() >= 30 || self.protocol.as_u8() < 28 {
            return Ok(None);
        }

        // Directories don't have hardlink dev/ino
        let is_dir = (mode & 0o170000) == 0o040000;
        if is_dir {
            return Ok(None);
        }

        // Read dev if not same as previous
        let dev = if flags.same_dev_pre30() {
            self.state.prev_hardlink_dev()
        } else {
            let raw_dev = crate::read_longint(reader)?;
            // Upstream stores dev + 1, so subtract 1
            let dev = raw_dev - 1;
            self.state.update_hardlink_dev(dev);
            dev
        };

        // Always read ino
        let ino = crate::read_longint(reader)?;

        Ok(Some((dev, ino)))
    }

    /// Reads checksum if always_checksum mode is enabled.
    ///
    /// Wire format: raw bytes of length flist_csum_len
    fn read_checksum<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        mode: u32,
    ) -> io::Result<Option<Vec<u8>>> {
        if !self.always_checksum || self.flist_csum_len == 0 {
            return Ok(None);
        }

        let is_regular = (mode & 0o170000) == 0o100000; // S_IFREG

        // For protocol < 28, non-regular files also have checksums (empty_sum)
        // For protocol >= 28, only regular files have checksums
        if !is_regular && self.protocol.as_u8() >= 28 {
            return Ok(None);
        }

        let mut checksum = vec![0u8; self.flist_csum_len];
        reader.read_exact(&mut checksum)?;

        // For non-regular files, the checksum is empty_sum (all zeros), don't store
        if !is_regular {
            return Ok(None);
        }

        Ok(Some(checksum))
    }

    /// Updates file list statistics based on the entry type.
    ///
    /// Tracks counts of files, directories, symlinks, devices, and special files,
    /// as well as total size for files and symlink targets.
    fn update_stats(&mut self, entry: &FileEntry) {
        if entry.is_dir() {
            self.stats.num_dirs += 1;
        } else if entry.is_file() {
            self.stats.num_files += 1;
            self.stats.total_size += entry.size();
        } else if entry.is_symlink() {
            self.stats.num_symlinks += 1;
            if let Some(target) = entry.link_target() {
                self.stats.total_size += target.as_os_str().len() as u64;
            }
        } else if entry.is_device() {
            self.stats.num_devices += 1;
        } else if entry.is_special() {
            self.stats.num_specials += 1;
        }
    }

    /// Reads hardlink index if preserving hardlinks and flags indicate it.
    ///
    /// Wire format (protocol 30+):
    /// - If XMIT_HLINKED is set but not XMIT_HLINK_FIRST: read varint index
    /// - If XMIT_HLINK_FIRST is also set: return u32::MAX (this is the first/leader)
    fn read_hardlink_idx<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<Option<u32>> {
        if !self.preserve_hard_links || self.protocol.as_u8() < 30 {
            return Ok(None);
        }

        // Check hardlink flags in extended byte
        let hlinked = (flags.extended & XMIT_HLINKED) != 0;
        if !hlinked {
            return Ok(None);
        }

        let hlink_first = (flags.extended & XMIT_HLINK_FIRST) != 0;
        if hlink_first {
            // This is the first/leader of the hardlink group
            return Ok(Some(u32::MAX));
        }

        // Read the index pointing to the leader
        let idx = read_varint(reader)? as u32;
        Ok(Some(idx))
    }

    /// Applies iconv encoding conversion to a filename.
    ///
    /// When `--iconv` is used, filenames are converted from the remote encoding
    /// to the local encoding. This enables interoperability between systems
    /// with different character encodings.
    fn apply_encoding_conversion(&self, name: Vec<u8>) -> io::Result<Vec<u8>> {
        if let Some(ref converter) = self.iconv {
            match converter.remote_to_local(&name) {
                Ok(converted) => Ok(converted.into_owned()),
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("filename encoding conversion failed: {e}"),
                )),
            }
        } else {
            Ok(name)
        }
    }

    /// Returns true if this entry is a hardlink follower (metadata was skipped on wire).
    ///
    /// A hardlink follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST.
    /// Such entries reference another entry in the file list, so their metadata
    /// (size, mtime, mode, uid, gid, symlink, rdev) was omitted from the wire.
    #[inline]
    fn is_hardlink_follower(&self, flags: FileFlags) -> bool {
        flags.hlinked() && !flags.hlink_first()
    }

    /// Reads the next file entry from the stream.
    ///
    /// Wire format order (matching upstream rsync flist.c recv_file_entry):
    /// 1. Flags
    /// 2. Name (with prefix compression)
    /// 3. Hardlink index (if follower) - then STOP for followers
    /// 4. File size
    /// 5. Mtime (if not XMIT_SAME_TIME)
    /// 6. Nsec (if XMIT_MOD_NSEC)
    /// 7. Crtime (if preserving, not XMIT_CRTIME_EQ_MTIME)
    /// 8. Mode (if not XMIT_SAME_MODE)
    /// 9. Atime (if preserving, non-dir, not XMIT_SAME_ATIME)
    /// 10. UID + user name (if preserving, not XMIT_SAME_UID)
    /// 11. GID + group name (if preserving, not XMIT_SAME_GID)
    /// 12. Device numbers (if device/special file)
    /// 13. Symlink target (if symlink)
    ///
    /// Returns `None` when the end-of-list marker is received (a zero byte).
    /// Returns an error on I/O failure or malformed data.
    ///
    /// # Upstream Reference
    ///
    /// See `flist.c:recv_file_entry()` lines 760-1050 for the complete wire decoding.
    pub fn read_entry<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<Option<FileEntry>> {
        // Step 1: Read and validate flags
        let flags = match self.read_flags(reader)? {
            FlagsResult::EndOfList => return Ok(None),
            FlagsResult::IoError(code) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("file list I/O error: {code}"),
                ));
            }
            FlagsResult::Flags(f) => f,
        };

        // Step 2: Read name with compression
        let name = self.read_name(reader, flags)?;

        // Step 3: Read hardlink index (MUST come immediately after name)
        // For hardlink followers, this is the only field read after the name.
        // Upstream rsync does "goto create_object" after reading the index for followers.
        let hardlink_idx = self.read_hardlink_idx(reader, flags)?;

        // Step 4+: Read metadata (unless this is a hardlink follower)
        // Hardlink followers have their metadata copied from the leader entry,
        // so we skip reading size, mtime, mode, uid, gid, symlink, and rdev.
        let (size, metadata, link_target, rdev, hardlink_dev_ino, checksum) =
            if self.is_hardlink_follower(flags) {
                // Use default values for hardlink follower - caller should copy from leader
                (
                    0u64,
                    MetadataResult {
                        mtime: 0,
                        nsec: 0,
                        mode: 0,
                        uid: None,
                        gid: None,
                        user_name: None,
                        group_name: None,
                        atime: None,
                        crtime: None,
                        content_dir: true,
                    },
                    None,
                    None,
                    None,
                    None,
                )
            } else {
                // Step 4: Read file size
                let size = self.read_size(reader)?;

                // Step 5: Read metadata fields (mtime, nsec, crtime, mode, atime, uid, gid)
                let metadata = self.read_metadata(reader, flags)?;

                // Step 6: Read device numbers (if applicable)
                // Also reads dummy rdev for special files in protocol < 31
                let rdev = self.read_rdev(reader, metadata.mode, flags)?;

                // Step 7: Read symlink target (if applicable)
                let link_target = self.read_symlink_target(reader, metadata.mode)?;

                // Step 8: Read hardlink dev/ino for protocol 28-29
                let hardlink_dev_ino = self.read_hardlink_dev_ino(reader, flags, metadata.mode)?;

                // Step 9: Read checksum if always_checksum mode
                let checksum = self.read_checksum(reader, metadata.mode)?;

                (
                    size,
                    metadata,
                    link_target,
                    rdev,
                    hardlink_dev_ino,
                    checksum,
                )
            };

        // Step 8: Apply encoding conversion
        let converted_name = self.apply_encoding_conversion(name)?;

        // Step 9: Construct entry from raw bytes (avoids UTF-8 validation on Unix)
        let mut entry = FileEntry::from_raw_bytes(
            converted_name,
            size,
            metadata.mode,
            metadata.mtime,
            metadata.nsec,
            flags,
        );

        // Step 10: Set symlink target if present
        if let Some(target) = link_target {
            entry.set_link_target(target);
        }

        // Step 11: Set device numbers if present
        if let Some((major, minor)) = rdev {
            entry.set_rdev(major, minor);
        }

        // Step 12: Set hardlink index if present
        if let Some(idx) = hardlink_idx {
            entry.set_hardlink_idx(idx);
        }

        // Step 13: Set UID if present
        if let Some(uid) = metadata.uid {
            entry.set_uid(uid);
        }

        // Step 14: Set GID if present
        if let Some(gid) = metadata.gid {
            entry.set_gid(gid);
        }

        // Step 15: Set user name if present
        if let Some(name) = metadata.user_name {
            entry.set_user_name(name);
        }

        // Step 16: Set group name if present
        if let Some(name) = metadata.group_name {
            entry.set_group_name(name);
        }

        // Step 17: Set atime if present
        if let Some(atime) = metadata.atime {
            entry.set_atime(atime);
        }

        // Step 18: Set crtime if present
        if let Some(crtime) = metadata.crtime {
            entry.set_crtime(crtime);
        }

        // Step 19: Set content_dir for directories
        if entry.is_dir() {
            entry.set_content_dir(metadata.content_dir);
        }

        // Step 20: Set hardlink dev/ino if present (protocol 28-29)
        if let Some((dev, ino)) = hardlink_dev_ino {
            entry.set_hardlink_dev(dev);
            entry.set_hardlink_ino(ino);
        }

        // Step 21: Set checksum if present
        if let Some(sum) = checksum {
            entry.set_checksum(sum);
        }

        // Step 22: Update statistics
        self.update_stats(&entry);

        Ok(Some(entry))
    }

    /// Reads the file name with path compression.
    ///
    /// The rsync wire format compresses file names by sharing a common prefix
    /// with the previous entry. If `XMIT_SAME_NAME` is set, a `same_len` byte
    /// indicates how many bytes to reuse from the previous name.
    ///
    /// # Wire Format
    ///
    /// - If `XMIT_SAME_NAME`: read u8 as `same_len`
    /// - If `XMIT_LONG_NAME`: read varint as `suffix_len`, else read u8
    /// - Read `suffix_len` bytes as the name suffix
    /// - Concatenate: `prev_name[..same_len] + suffix`
    fn read_name<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<Vec<u8>> {
        // Determine shared prefix length
        let same_len = if flags.same_name() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        } else {
            0
        };

        // Read suffix length
        let suffix_len = if flags.long_name() {
            read_varint(reader)? as usize
        } else {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        };

        // Validate lengths
        if same_len > self.state.prev_name().len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "same_len {} exceeds previous name length {}",
                    same_len,
                    self.state.prev_name().len()
                ),
            ));
        }

        // Build full name
        let mut name = Vec::with_capacity(same_len + suffix_len);
        name.extend_from_slice(&self.state.prev_name()[..same_len]);

        if suffix_len > 0 {
            let start = name.len();
            name.resize(start + suffix_len, 0);
            reader.read_exact(&mut name[start..])?;
        }

        // Update state
        self.state.update_name(&name);

        Ok(name)
    }

    /// Reads the file size using protocol-appropriate encoding.
    ///
    /// The encoding varies by protocol version:
    /// - Protocol < 30: Fixed 32-bit or 64-bit encoding
    /// - Protocol 30+: Variable-length encoding (varlong30)
    fn read_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<u64> {
        let size = self.codec.read_file_size(reader)?;
        Ok(size as u64)
    }
}

/// Reads a single file entry from a reader.
///
/// This is a convenience function for reading individual entries without
/// maintaining reader state. For reading multiple entries, use [`FileListReader`]
/// to benefit from cross-entry compression.
///
/// # Returns
///
/// - `Ok(Some(entry))` - Successfully read a file entry
/// - `Ok(None)` - End of file list marker received
/// - `Err(_)` - I/O or protocol error
pub fn read_file_entry<R: Read>(
    reader: &mut R,
    protocol: ProtocolVersion,
) -> io::Result<Option<FileEntry>> {
    let mut list_reader = FileListReader::new(protocol);
    list_reader.read_entry(reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_protocol() -> ProtocolVersion {
        ProtocolVersion::try_from(32u8).unwrap()
    }

    #[test]
    fn read_end_of_list_marker() {
        let data = [0u8];
        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(test_protocol());

        let result = reader.read_entry(&mut cursor).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_simple_entry() {
        use super::super::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("test".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test");
        assert_eq!(read_entry.size(), 100);
        assert_eq!(read_entry.mode(), 0o100644);
        assert_eq!(read_entry.mtime(), 1700000000);
    }

    #[test]
    fn read_entry_with_name_compression() {
        use super::super::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry1 = FileEntry::new_file("dir/file".into(), 50, 0o100644);
        entry1.set_mtime(1700000000, 0);

        let mut entry2 = FileEntry::new_file("dir/other".into(), 75, 0o100644);
        entry2.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry1).unwrap();
        writer.write_entry(&mut data, &entry2).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry1.name(), "dir/file");

        let read_entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry2.name(), "dir/other");
    }

    #[test]
    fn read_entry_detects_error_marker_with_safe_file_list() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        let error_code = 42;

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);
        encode_varint_to_vec(error_code, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("file list I/O error: 42"));
    }

    #[test]
    fn read_entry_rejects_error_marker_without_safe_file_list() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("Invalid flist flag"));
    }

    #[test]
    fn read_entry_with_protocol_31_accepts_error_marker() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        let error_code = 99;

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);
        encode_varint_to_vec(error_code, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("file list I/O error: 99"));
    }

    #[test]
    fn read_write_round_trip_with_safe_file_list_error_nonvarint() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let flags = CompatibilityFlags::SAFE_FILE_LIST;

        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut data = Vec::new();
        writer.write_end(&mut data, Some(123)).unwrap();

        let mut reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("file list I/O error: 123"));
    }

    #[test]
    fn read_write_round_trip_with_varint_end_marker() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;

        // Test end marker with io_error=0 returns Ok(None)
        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut data = Vec::new();
        writer.write_end(&mut data, Some(0)).unwrap();

        let mut reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
        assert_eq!(cursor.position() as usize, data.len());

        // Test end marker with non-zero error returns Err
        let mut data2 = Vec::new();
        writer.write_end(&mut data2, Some(123)).unwrap();

        let mut reader2 = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor2 = Cursor::new(&data2[..]);
        let result2 = reader2.read_entry(&mut cursor2);
        assert!(result2.is_err());
        let err = result2.unwrap_err();
        assert!(err.to_string().contains("123"));
    }

    // Tests for extracted helper methods

    #[test]
    fn use_varint_flags_checks_compat_flags() {
        let protocol = test_protocol();

        let reader_without = FileListReader::new(protocol);
        assert!(!reader_without.use_varint_flags());

        let reader_with =
            FileListReader::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
        assert!(reader_with.use_varint_flags());
    }

    #[test]
    fn use_safe_file_list_checks_protocol_and_flags() {
        // Protocol 30 without flag
        let reader30 = FileListReader::new(ProtocolVersion::try_from(30u8).unwrap());
        assert!(!reader30.use_safe_file_list());

        // Protocol 30 with flag
        let reader30_safe = FileListReader::with_compat_flags(
            ProtocolVersion::try_from(30u8).unwrap(),
            CompatibilityFlags::SAFE_FILE_LIST,
        );
        assert!(reader30_safe.use_safe_file_list());

        // Protocol 31+ automatically enables safe mode
        let reader31 = FileListReader::new(ProtocolVersion::try_from(31u8).unwrap());
        assert!(reader31.use_safe_file_list());
    }

    #[test]
    fn read_flags_returns_end_of_list_for_zero() {
        let reader = FileListReader::new(test_protocol());
        let data = [0u8];
        let mut cursor = Cursor::new(&data[..]);

        match reader.read_flags(&mut cursor).unwrap() {
            FlagsResult::EndOfList => {}
            other => panic!("expected EndOfList, got {other:?}"),
        }
    }

    #[test]
    fn read_flags_returns_io_error_in_varint_mode() {
        let reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        // Zero flags followed by non-zero error code
        use crate::varint::encode_varint_to_vec;
        let mut data = Vec::new();
        encode_varint_to_vec(0, &mut data); // flags = 0
        encode_varint_to_vec(42, &mut data); // error = 42

        let mut cursor = Cursor::new(&data[..]);

        match reader.read_flags(&mut cursor).unwrap() {
            FlagsResult::IoError(code) => assert_eq!(code, 42),
            other => panic!("expected IoError(42), got {other:?}"),
        }
    }

    #[test]
    fn is_hardlink_follower_helper() {
        use crate::flist::flags::{XMIT_HLINK_FIRST, XMIT_HLINKED};

        let reader = FileListReader::new(test_protocol()).with_preserve_hard_links(true);

        // No hardlink flags
        let flags_none = FileFlags::new(0, 0);
        assert!(!reader.is_hardlink_follower(flags_none));

        // Leader (HLINKED + HLINK_FIRST)
        let flags_leader = FileFlags::new(0, XMIT_HLINKED | XMIT_HLINK_FIRST);
        assert!(!reader.is_hardlink_follower(flags_leader));

        // Follower (HLINKED only, no HLINK_FIRST)
        let flags_follower = FileFlags::new(0, XMIT_HLINKED);
        assert!(reader.is_hardlink_follower(flags_follower));
    }

    #[test]
    fn read_write_round_trip_with_atime() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer =
            FileListWriter::with_compat_flags(protocol, flags).with_preserve_atimes(true);

        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        entry.set_atime(1700001000);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader =
            FileListReader::with_compat_flags(protocol, flags).with_preserve_atimes(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.atime(), 1700001000);
    }

    #[test]
    fn read_write_round_trip_with_same_atime() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer =
            FileListWriter::with_compat_flags(protocol, flags).with_preserve_atimes(true);

        // First file with atime
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o100644);
        entry1.set_mtime(1700000000, 0);
        entry1.set_atime(1700001000);

        // Second file with same atime (should use XMIT_SAME_ATIME flag)
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o100644);
        entry2.set_mtime(1700000000, 0);
        entry2.set_atime(1700001000);

        writer.write_entry(&mut data, &entry1).unwrap();
        writer.write_entry(&mut data, &entry2).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader =
            FileListReader::with_compat_flags(protocol, flags).with_preserve_atimes(true);

        let read_entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry1.atime(), 1700001000);

        let read_entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry2.atime(), 1700001000);
    }

    #[test]
    fn read_write_round_trip_with_crtime() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer =
            FileListWriter::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        entry.set_crtime(1699999000); // Different from mtime

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader =
            FileListReader::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.crtime(), 1699999000);
    }

    #[test]
    fn read_write_round_trip_with_crtime_eq_mtime() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer =
            FileListWriter::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

        // crtime equals mtime - should use XMIT_CRTIME_EQ_MTIME flag
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        entry.set_crtime(1700000000); // Same as mtime

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader =
            FileListReader::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.crtime(), 1700000000);
        assert_eq!(read_entry.crtime(), read_entry.mtime());
    }

    #[test]
    fn read_write_round_trip_directory_with_content() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer = FileListWriter::with_compat_flags(protocol, flags);

        // Directory with content
        let mut entry = FileEntry::new_directory("mydir".into(), 0o040755);
        entry.set_mtime(1700000000, 0);
        entry.set_content_dir(true);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "mydir");
        assert!(read_entry.is_dir());
        assert!(read_entry.content_dir());
    }

    #[test]
    fn read_write_round_trip_directory_without_content() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer = FileListWriter::with_compat_flags(protocol, flags);

        // Directory without content (implied directory)
        let mut entry = FileEntry::new_directory("implied_dir".into(), 0o040755);
        entry.set_mtime(1700000000, 0);
        entry.set_content_dir(false);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "implied_dir");
        assert!(read_entry.is_dir());
        assert!(!read_entry.content_dir());
    }

    #[test]
    fn read_write_round_trip_with_all_times() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();
        let mut writer = FileListWriter::with_compat_flags(protocol, flags)
            .with_preserve_atimes(true)
            .with_preserve_crtimes(true);

        let mut entry = FileEntry::new_file("complete.txt".into(), 500, 0o100644);
        entry.set_mtime(1700000000, 0);
        entry.set_atime(1700001000);
        entry.set_crtime(1699990000);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(protocol, flags)
            .with_preserve_atimes(true)
            .with_preserve_crtimes(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "complete.txt");
        assert_eq!(read_entry.mtime(), 1700000000);
        assert_eq!(read_entry.atime(), 1700001000);
        assert_eq!(read_entry.crtime(), 1699990000);
    }

    #[test]
    fn preserve_atimes_builder() {
        let reader = FileListReader::new(test_protocol()).with_preserve_atimes(true);
        assert!(reader.preserve_atimes);
    }

    #[test]
    fn preserve_crtimes_builder() {
        let reader = FileListReader::new(test_protocol()).with_preserve_crtimes(true);
        assert!(reader.preserve_crtimes);
    }

    // Protocol 28/29 specific tests for rdev handling

    #[test]
    fn read_device_entry_protocol_29_byte_minor() {
        use super::super::write::FileListWriter;

        // Protocol 29 uses different minor encoding based on XMIT_RDEV_MINOR_8_pre30 flag
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        // Block device with small minor (fits in byte)
        let mut entry = FileEntry::new_block_device("dev/sda".into(), 0o644, 8, 0);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "dev/sda");
        assert_eq!(read_entry.rdev_major(), Some(8));
        assert_eq!(read_entry.rdev_minor(), Some(0));
    }

    #[test]
    fn read_device_entry_protocol_29_int_minor() {
        use super::super::write::FileListWriter;

        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        // Block device with large minor (needs 4-byte int)
        let mut entry = FileEntry::new_block_device("dev/nvme0n1".into(), 0o644, 259, 65536);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "dev/nvme0n1");
        assert_eq!(read_entry.rdev_major(), Some(259));
        assert_eq!(read_entry.rdev_minor(), Some(65536));
    }

    #[test]
    fn read_device_entry_protocol_28_same_major_optimization() {
        use super::super::write::FileListWriter;

        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        // Two devices with same major - tests XMIT_SAME_RDEV_MAJOR flag
        let mut entry1 = FileEntry::new_block_device("dev/sda1".into(), 0o644, 8, 1);
        entry1.set_mtime(1700000000, 0);

        let mut entry2 = FileEntry::new_block_device("dev/sda2".into(), 0o644, 8, 2);
        entry2.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry1).unwrap();
        writer.write_entry(&mut data, &entry2).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read1.rdev_major(), Some(8));
        assert_eq!(read1.rdev_minor(), Some(1));

        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read2.rdev_major(), Some(8));
        assert_eq!(read2.rdev_minor(), Some(2));
    }

    #[test]
    fn read_device_entry_protocol_30_uses_varint_minor() {
        use super::super::write::FileListWriter;

        // Protocol 30+ uses varint for minor
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        let mut entry = FileEntry::new_block_device("dev/loop0".into(), 0o644, 7, 12345);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.rdev_major(), Some(7));
        assert_eq!(read_entry.rdev_minor(), Some(12345));
    }

    #[test]
    fn read_name_rejects_invalid_prefix_length() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_SAME_NAME;
        use crate::varint::encode_varint_to_vec;

        // This tests the error path at read_name() lines 1025-1034
        // where same_len > prev_name.len() causes an error.

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        // Craft data with XMIT_SAME_NAME flag set but with same_len > prev_name.len()
        // Since prev_name starts empty (len=0), any same_len > 0 will trigger the error.
        let mut data = Vec::new();

        // Flags: XMIT_SAME_NAME (0x20) - indicates name compression
        let xmit_flags = XMIT_SAME_NAME;
        encode_varint_to_vec(xmit_flags as i32, &mut data);

        // same_len byte: 5 (but prev_name is empty, so this is invalid)
        data.push(5u8);

        // suffix_len byte: 4 (name = "test")
        data.push(4u8);

        // suffix data: "test"
        data.extend_from_slice(b"test");

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let result = reader.read_entry(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds previous name length"));
    }

    #[test]
    fn read_entry_truncated_name_fails() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Test truncated name data (suffix_len claims more bytes than available)
        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut data = Vec::new();

        // Flags: 0x01 (minimal valid flag that isn't end-of-list)
        encode_varint_to_vec(0x01, &mut data);

        // suffix_len byte: 100 (but we only provide 4 bytes)
        data.push(100u8);

        // suffix data: only "test" (4 bytes, not 100)
        data.extend_from_slice(b"test");

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let result = reader.read_entry(&mut cursor);
        // Error can be UnexpectedEof or InvalidData depending on where truncation is detected
        assert!(result.is_err(), "Expected error for truncated name data");
    }

    // =========================================================================
    // Truncated Wire Format Tests
    //
    // These tests verify proper error handling when the wire format data is
    // incomplete/truncated at various points. All should return UnexpectedEof
    // errors with appropriate context.
    // =========================================================================

    /// Helper to assert UnexpectedEof error from truncated data
    fn assert_unexpected_eof(result: io::Result<Option<FileEntry>>, context: &str) {
        match result {
            Err(e) => {
                assert_eq!(
                    e.kind(),
                    io::ErrorKind::UnexpectedEof,
                    "{}: expected UnexpectedEof, got {:?}",
                    context,
                    e.kind()
                );
            }
            Ok(entry) => {
                panic!(
                    "{}: expected UnexpectedEof error, got Ok({:?})",
                    context,
                    entry.map(|e| e.name().to_string())
                );
            }
        }
    }

    #[test]
    fn truncated_empty_input() {
        // Empty input should fail when trying to read flags
        let data: &[u8] = &[];
        let mut cursor = Cursor::new(data);
        let mut reader = FileListReader::new(test_protocol());

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "empty input");
    }

    #[test]
    fn truncated_flags_byte_nonvarint() {
        // For non-varint mode, flags are a single byte - empty input truncates this
        let data: &[u8] = &[];
        let mut cursor = Cursor::new(data);
        let mut reader = FileListReader::new(test_protocol());
        // Default is non-varint mode

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated flags byte (non-varint)");
    }

    #[test]
    fn truncated_flags_varint_incomplete() {
        use crate::CompatibilityFlags;

        // In varint mode, a multi-byte varint that's cut short
        // Varint encoding: 0x80 indicates continuation needed
        let data: &[u8] = &[0x80]; // Incomplete varint (continuation bit set but no more bytes)
        let mut cursor = Cursor::new(data);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated varint flags");
    }

    #[test]
    fn truncated_extended_flags_byte() {
        // When XMIT_EXTENDED_FLAGS (0x40) is set, need an extra byte
        use crate::flist::flags::XMIT_EXTENDED_FLAGS;

        let data: &[u8] = &[XMIT_EXTENDED_FLAGS]; // Extended flags bit set but no extra byte
        let mut cursor = Cursor::new(data);
        let mut reader = FileListReader::new(test_protocol());

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated extended flags byte");
    }

    #[test]
    fn truncated_name_length_byte() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid flags followed by no name length
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        // Missing: name length byte

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated name length byte");
    }

    #[test]
    fn truncated_name_data_partial() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid flags + name length of 10, but only 3 bytes of name data
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(10u8); // Name length: 10 bytes
        data.extend_from_slice(b"abc"); // Only 3 bytes provided

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated name data (partial)");
    }

    #[test]
    fn truncated_same_name_prefix_byte() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_SAME_NAME;
        use crate::varint::encode_varint_to_vec;

        // XMIT_SAME_NAME flag set but no prefix length byte
        let mut data = Vec::new();
        encode_varint_to_vec(XMIT_SAME_NAME as i32, &mut data);
        // Missing: same_len byte

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated same_name prefix byte");
    }

    #[test]
    fn truncated_size_field() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid flags + complete name, but truncated size
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length: 4
        data.extend_from_slice(b"test"); // Complete name
        // Missing: size field (varlong)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated size field");
    }

    #[test]
    fn truncated_size_field_partial_varlong() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid entry up to size, but size varlong is incomplete
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length: 4
        data.extend_from_slice(b"test"); // Complete name
        data.push(0xFF); // Start of varlong indicating large value, but incomplete

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated size field (partial varlong)");
    }

    #[test]
    fn truncated_mtime_field() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid entry up to size, but missing mtime
        // When XMIT_SAME_TIME is NOT set, mtime must be read
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_SAME_TIME)
        data.push(4u8); // Name length
        data.extend_from_slice(b"test"); // Name
        data.push(100u8); // Size = 100 (simple varlong)
        // Missing: mtime field

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated mtime field");
    }

    #[test]
    fn truncated_mode_field() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid entry up to mtime, but missing mode (4 bytes LE)
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_SAME_MODE)
        data.push(4u8); // Name length
        data.extend_from_slice(b"test"); // Name
        data.push(100u8); // Size = 100
        data.push(0u8); // mtime varlong (small value)
        // Missing: mode field (4 bytes)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated mode field");
    }

    #[test]
    fn truncated_mode_field_partial() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Valid entry up to mtime, but mode is only 2 of 4 bytes
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length
        data.extend_from_slice(b"test"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&[0x44, 0x81]); // Partial mode (only 2 bytes of 4)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated mode field (partial)");
    }

    #[test]
    fn truncated_uid_field_with_preserve_uid() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Entry with preserve_uid enabled, but UID field is missing
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_SAME_UID)
        data.push(4u8); // Name length
        data.extend_from_slice(b"test"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode (regular file)
        // Missing: UID field

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_uid(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated uid field");
    }

    #[test]
    fn truncated_gid_field_with_preserve_gid() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Entry with preserve_gid enabled, but GID field is missing
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_SAME_GID)
        data.push(4u8); // Name length
        data.extend_from_slice(b"test"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode
        // Missing: GID field

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_gid(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated gid field");
    }

    #[test]
    fn truncated_symlink_target_length() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Symlink entry but target length is missing
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length
        data.extend_from_slice(b"link"); // Name
        data.push(0u8); // Size = 0 (symlinks have size 0)
        data.push(0u8); // mtime
        data.extend_from_slice(&0o120777u32.to_le_bytes()); // Mode (symlink)
        // Missing: symlink target length

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_links(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated symlink target length");
    }

    #[test]
    fn truncated_symlink_target_data() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Symlink entry with target length but truncated target data
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length
        data.extend_from_slice(b"link"); // Name
        data.push(0u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o120777u32.to_le_bytes()); // Mode (symlink)
        data.push(20u8); // Target length: 20 bytes
        data.extend_from_slice(b"/etc"); // Only 4 bytes of 20

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_links(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated symlink target data");
    }

    #[test]
    fn truncated_device_major() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Block device entry but missing rdev major
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_SAME_RDEV_MAJOR)
        data.push(7u8); // Name length
        data.extend_from_slice(b"dev/sda"); // Name
        data.push(0u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o060644u32.to_le_bytes()); // Mode (block device)
        // Missing: rdev major (varint30)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_devices(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated device major");
    }

    #[test]
    fn truncated_device_minor() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Block device entry with major but missing minor
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(7u8); // Name length
        data.extend_from_slice(b"dev/sda"); // Name
        data.push(0u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o060644u32.to_le_bytes()); // Mode (block device)
        data.push(8u8); // rdev major = 8
        // Missing: rdev minor (varint)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_devices(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated device minor");
    }

    #[test]
    fn truncated_atime_field() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // File entry with preserve_atimes but atime is missing
        // Note: atime only applies to non-directories
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_SAME_ATIME)
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode (regular file, not dir)
        // Missing: atime field

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_atimes(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated atime field");
    }

    #[test]
    fn truncated_checksum_field() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // File entry with always_checksum but checksum is missing
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode (regular file)
        // Missing: checksum (16 bytes for MD5)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_always_checksum(16); // MD5 = 16 bytes

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated checksum field");
    }

    #[test]
    fn truncated_checksum_field_partial() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // File entry with checksum but only partial data
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode (regular file)
        data.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x12]); // Only 4 bytes of 16-byte checksum

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_always_checksum(16);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated checksum field (partial)");
    }

    #[test]
    fn truncated_hardlink_index() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_HLINKED;
        use crate::varint::encode_varint_to_vec;

        // Hardlink follower entry but index is missing
        // XMIT_HLINKED without XMIT_HLINK_FIRST means follower
        let flags_value = (0x01) | ((XMIT_HLINKED as i32) << 8);
        let mut data = Vec::new();
        encode_varint_to_vec(flags_value, &mut data);
        data.push(4u8); // Name length
        data.extend_from_slice(b"link"); // Name
        // Missing: hardlink index (varint)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_hard_links(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated hardlink index");
    }

    #[test]
    fn truncated_user_name_length() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_USER_NAME_FOLLOWS;
        use crate::varint::encode_varint_to_vec;

        // Entry with XMIT_USER_NAME_FOLLOWS but name length missing
        let flags_value = (0x01) | ((XMIT_USER_NAME_FOLLOWS as i32) << 8);
        let mut data = Vec::new();
        encode_varint_to_vec(flags_value, &mut data);
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode
        data.push(100u8); // UID as varint (small value)
        // Missing: user name length byte

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_uid(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated user name length");
    }

    #[test]
    fn truncated_user_name_data() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_USER_NAME_FOLLOWS;
        use crate::varint::encode_varint_to_vec;

        // Entry with user name but truncated name data
        let flags_value = (0x01) | ((XMIT_USER_NAME_FOLLOWS as i32) << 8);
        let mut data = Vec::new();
        encode_varint_to_vec(flags_value, &mut data);
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        data.extend_from_slice(&0o100644u32.to_le_bytes()); // Mode
        data.push(100u8); // UID varint
        data.push(10u8); // User name length: 10
        data.extend_from_slice(b"user"); // Only 4 bytes of 10

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_uid(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated user name data");
    }

    #[test]
    fn truncated_crtime_field() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Entry with preserve_crtimes but crtime is missing
        // XMIT_CRTIME_EQ_MTIME not set means crtime must be read
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags (no XMIT_CRTIME_EQ_MTIME)
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        // Missing: crtime field (read before mode when preserve_crtimes)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        )
        .with_preserve_crtimes(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated crtime field");
    }

    #[test]
    fn truncated_nsec_field() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_MOD_NSEC;
        use crate::varint::encode_varint_to_vec;

        // Entry with XMIT_MOD_NSEC but nsec field is missing
        // Protocol 31+ supports nanoseconds
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let flags_value = (0x01) | ((XMIT_MOD_NSEC as i32) << 8);
        let mut data = Vec::new();
        encode_varint_to_vec(flags_value, &mut data);
        data.push(4u8); // Name length
        data.extend_from_slice(b"file"); // Name
        data.push(100u8); // Size
        data.push(0u8); // mtime
        // Missing: nsec field (varint30)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            protocol,
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated nsec field");
    }

    #[test]
    fn truncated_long_name_varint() {
        use crate::CompatibilityFlags;
        use crate::flist::flags::XMIT_LONG_NAME;
        use crate::varint::encode_varint_to_vec;

        // Entry with XMIT_LONG_NAME but varint for name length is incomplete
        let flags_value = XMIT_LONG_NAME as i32 | 0x01;
        let mut data = Vec::new();
        encode_varint_to_vec(flags_value, &mut data);
        data.push(0x80); // Incomplete varint (continuation bit set)
        // Missing: rest of varint

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated long name varint");
    }

    #[test]
    fn truncated_protocol_29_device_minor_int() {
        use super::super::write::FileListWriter;

        // Protocol 29 uses 4-byte int for large minors (when > 255)
        // Generate a complete entry with the writer, then truncate the last 2 bytes
        // (the minor field for large values is 4 bytes, truncating to 2)
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

        // Block device with large minor (needs 4-byte int, not 1-byte)
        let mut entry = FileEntry::new_block_device("dev/nvme0n1".into(), 0o644, 259, 65536);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        // Truncate the last 2 bytes (partial 4-byte minor)
        let truncated_data = &data[..data.len() - 2];

        let mut cursor = Cursor::new(truncated_data);
        let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated protocol 29 device minor (int)");
    }
}
