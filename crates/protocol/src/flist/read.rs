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
use std::path::{Path, PathBuf};

use logging::debug_log;

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::acl::{AclCache, receive_acl_cached};
use crate::codec::{ProtocolCodec, ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::varint::{read_varint, read_varint30_int};
use crate::xattr::XattrCache;

use super::entry::FileEntry;
use super::flags::{
    FileFlags, XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST,
    XMIT_NO_CONTENT_DIR,
};
use super::intern::PathInterner;
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
    /// Whether to preserve (and thus read) device numbers (block/char) from the wire.
    preserve_devices: bool,
    /// Whether to preserve (and thus read) special files (FIFOs/sockets) from the wire.
    preserve_specials: bool,
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
    /// Xattr preservation level (1 = normal, 2 = include rsync.% internal attrs).
    ///
    /// Only meaningful when `preserve_xattrs` is true. Corresponds to the number
    /// of `-X` flags passed on the command line. Level 2 preserves rsync-internal
    /// attributes like `rsync.%stat` and `rsync.%aacl`.
    xattr_level: u32,
    /// Whether the receiver has root privileges.
    ///
    /// Affects xattr namespace handling during receive - root can write to
    /// non-user namespaces (security, trusted, system) directly.
    am_root: bool,
    /// Length of checksum to read (depends on protocol and checksum algorithm).
    flist_csum_len: usize,
    /// Optional filename encoding converter (for --iconv support).
    iconv: Option<FilenameConverter>,
    /// Whether `--relative` (`-R`) paths are active.
    ///
    /// Controls pathname validation: when false, absolute paths (leading `/`)
    /// are rejected. When true, leading slashes are stripped instead.
    /// upstream: flist.c:757 `!relative_paths && *thisname == '/'`
    relative_paths: bool,
    /// Interner for deduplicating parent directory paths across file entries.
    ///
    /// When many entries share the same parent directory, the interner ensures
    /// they all point to a single `Arc<Path>` allocation instead of each holding
    /// an independent copy.
    dirname_interner: PathInterner,
    /// Accumulated I/O error code from the sender.
    ///
    /// Upstream `flist.c:recv_file_list()` does `io_error |= err` when it
    /// encounters an IoError marker, then breaks the loop. We mirror this by
    /// returning `Ok(None)` and accumulating the error here for the caller to
    /// inspect after the file list is fully read.
    io_error: i32,
    /// ACL cache for tracking received ACL definitions.
    ///
    /// When `preserve_acls` is enabled, ACL data is read from the wire after
    /// the checksum for each entry. Literal ACL definitions are stored in this
    /// cache and referenced by index. Upstream uses `access_acl_list` and
    /// `default_acl_list` globals - we encapsulate them in `AclCache`.
    acl_cache: AclCache,
    /// Cache of received xattr sets, indexed by position.
    ///
    /// Populated during file list reading when `preserve_xattrs` is true.
    /// Each file entry stores an index into this cache rather than duplicating
    /// the full xattr list. Mirrors upstream rsync's `rsync_xal_l`.
    xattr_cache: XattrCache,
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
    /// Nanosecond component of access time (protocol 32+, --atimes).
    atime_nsec: u32,
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
            preserve_specials: false,
            preserve_hard_links: false,
            preserve_atimes: false,
            preserve_crtimes: false,
            always_checksum: false,
            preserve_acls: false,
            preserve_xattrs: false,
            xattr_level: 0,
            am_root: false,
            flist_csum_len: 0,
            iconv: None,
            relative_paths: false,
            dirname_interner: PathInterner::new(),
            io_error: 0,
            acl_cache: AclCache::new(),
            xattr_cache: XattrCache::new(),
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
            preserve_specials: false,
            preserve_hard_links: false,
            preserve_atimes: false,
            preserve_crtimes: false,
            always_checksum: false,
            preserve_acls: false,
            preserve_xattrs: false,
            xattr_level: 0,
            am_root: false,
            flist_csum_len: 0,
            iconv: None,
            relative_paths: false,
            dirname_interner: PathInterner::new(),
            io_error: 0,
            acl_cache: AclCache::new(),
            xattr_cache: XattrCache::new(),
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

    /// Sets whether device numbers (block/char) should be read from the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Sets whether special files (FIFOs/sockets) should be read from the wire.
    ///
    /// # Upstream Reference
    ///
    /// Upstream `flist.c` checks `preserve_specials` separately from
    /// `preserve_devices` for `IS_SPECIAL()` file types.
    #[inline]
    #[must_use]
    pub const fn with_preserve_specials(mut self, preserve: bool) -> Self {
        self.preserve_specials = preserve;
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
        if preserve && self.xattr_level == 0 {
            self.xattr_level = 1;
        }
        self
    }

    /// Sets the xattr preservation level.
    ///
    /// Level 1 is normal xattr preservation (`-X`). Level 2 also preserves
    /// rsync-internal attributes like `rsync.%stat` (`-XX`).
    #[inline]
    #[must_use]
    pub const fn with_xattr_level(mut self, level: u32) -> Self {
        self.xattr_level = level;
        if level > 0 {
            self.preserve_xattrs = true;
        }
        self
    }

    /// Sets whether the receiver has root privileges.
    ///
    /// Affects xattr namespace handling - root can access non-user namespaces.
    #[inline]
    #[must_use]
    pub const fn with_am_root(mut self, am_root: bool) -> Self {
        self.am_root = am_root;
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

    /// Sets whether `--relative` (`-R`) transfer mode is active.
    ///
    /// When enabled, leading slashes on received filenames are stripped instead
    /// of causing an abort. When disabled, absolute paths from the sender are
    /// rejected as unsafe.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:757`: `!relative_paths && *thisname == '/'`
    #[inline]
    #[must_use]
    pub const fn with_relative_paths(mut self, relative: bool) -> Self {
        self.relative_paths = relative;
        self
    }

    /// Returns the statistics collected during file list reading.
    #[must_use]
    pub const fn stats(&self) -> &FileListStats {
        &self.stats
    }

    /// Returns the accumulated I/O error code from the sender.
    ///
    /// Upstream `flist.c:recv_file_list()` accumulates `io_error |= err` when
    /// the sender reports I/O errors during file list generation. A non-zero
    /// value means some source files could not be read, but the transfer should
    /// still proceed with the files that were successfully listed.
    #[must_use]
    pub const fn io_error(&self) -> i32 {
        self.io_error
    }

    /// Returns a reference to the ACL cache.
    ///
    /// Callers can use this to look up ACL definitions by index after
    /// reading the file list.
    #[must_use]
    pub const fn acl_cache(&self) -> &AclCache {
        &self.acl_cache
    }

    /// Returns a reference to the xattr cache populated during file list reading.
    ///
    /// Each cached entry is an [`XattrList`](crate::xattr::XattrList) containing
    /// the xattr name-value pairs for one or more files. File entries reference
    /// these cached lists by index via [`FileEntry::xattr_ndx`].
    #[must_use]
    pub fn xattr_cache(&self) -> &XattrCache {
        &self.xattr_cache
    }

    /// Returns a mutable reference to the xattr cache.
    ///
    /// Needed during the data exchange phase when abbreviated xattr values
    /// are replaced with full values received from the sender.
    pub fn xattr_cache_mut(&mut self) -> &mut XattrCache {
        &mut self.xattr_cache
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

        // Level 4: Wire format bytes for flags
        debug_log!(
            Flist,
            4,
            "read_flags: raw={:#x} varint={}",
            flags_value,
            use_varint
        );

        // Check for end-of-list marker
        if flags_value == 0 {
            if use_varint {
                // In varint mode, error code follows zero flags
                let io_error = read_varint(reader)?;
                if io_error != 0 {
                    debug_log!(
                        Flist,
                        4,
                        "read_flags: end-of-list with io_error={}",
                        io_error
                    );
                    return Ok(FlagsResult::IoError(io_error));
                }
            }
            debug_log!(Flist, 4, "read_flags: end-of-list marker");
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
        // Level 4: Extended flags detail
        if ext_byte != 0 || ext16_byte != 0 {
            debug_log!(
                Flist,
                4,
                "read_flags: primary={:#x} ext={:#x} ext16={:#x}",
                primary_byte,
                ext_byte,
                ext16_byte
            );
        }

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
        // upstream: flist.c:828-839 — proto >= 30 uses read_varlong(f, 4),
        // proto < 30 uses read_uint(f) (fixed 4-byte unsigned)
        let mtime = if flags.same_time() {
            self.state.prev_mtime()
        } else {
            let mtime = self.codec.read_mtime(reader)?;
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
            let mode = super::wire_mode::from_wire_mode(i32::from_le_bytes(mode_bytes));
            self.state.update_mode(mode);
            mode
        };

        // Determine if this is a directory (needed for atime and content_dir)
        let is_dir = (mode & 0o170000) == 0o040000;

        // 5. Read atime if preserving atimes (AFTER mode, non-directories only)
        let (atime, atime_nsec) = if self.preserve_atimes && !is_dir {
            if flags.same_atime() {
                (Some(self.state.prev_atime()), 0)
            } else {
                let atime = crate::read_varlong(reader, 4)?;
                let nsec = if self.protocol.as_u8() >= 32 {
                    crate::read_varint(reader)? as u32
                } else {
                    0
                };
                self.state.update_atime(atime);
                (Some(atime), nsec)
            }
        } else {
            (None, 0)
        };

        // 6. Read UID and optional user name
        let (uid, user_name) = if self.preserve_uid {
            let (id, name) = read_owner_id(
                reader,
                flags.same_uid(),
                flags.user_name_follows(),
                self.state.prev_uid(),
                self.protocol.uses_fixed_encoding(),
            )?;
            self.state.update_uid(id);
            (Some(id), name)
        } else {
            (None, None)
        };

        // 7. Read GID and optional group name
        let (gid, group_name) = if self.preserve_gid {
            let (id, name) = read_owner_id(
                reader,
                flags.same_gid(),
                flags.group_name_follows(),
                self.state.prev_gid(),
                self.protocol.uses_fixed_encoding(),
            )?;
            self.state.update_gid(id);
            (Some(id), name)
        } else {
            (None, None)
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

        // Level 3: Encoding/decoding details for metadata
        debug_log!(
            Flist,
            3,
            "read_metadata: mtime={} nsec={} mode={:o} uid={:?} gid={:?}",
            mtime,
            nsec,
            mode,
            uid,
            gid
        );

        Ok(MetadataResult {
            mtime,
            nsec,
            mode,
            uid,
            gid,
            user_name,
            group_name,
            atime,
            atime_nsec,
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

        // upstream: rsync.h MAXPATHLEN — reject targets that exceed PATH_MAX to
        // prevent unbounded allocation from a malicious sender.
        if len > crate::wire::file_entry_decode::MAX_SYMLINK_TARGET_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "symlink target length {len} exceeds maximum {}",
                    crate::wire::file_entry_decode::MAX_SYMLINK_TARGET_LEN
                ),
            ));
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

        // upstream: flist.c checks preserve_devices for IS_DEVICE and
        // preserve_specials for IS_SPECIAL separately
        let needs_rdev = (self.preserve_devices && is_device)
            || (self.preserve_specials && is_special && self.protocol.as_u8() < 31);

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

    /// Cleans and validates a filename received from the sender.
    ///
    /// Mirrors upstream `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)` followed
    /// by the leading-slash check at flist.c:756-760. Performs in-place on a byte
    /// buffer to avoid allocations on the common (clean) path.
    ///
    /// Normalization:
    /// - Collapses duplicate slashes (`a//b` -> `a/b`)
    /// - Removes interior `.` components (`a/./b` -> `a/b`)
    /// - Strips trailing slashes (`a/b/` -> `a/b`)
    /// - Replaces empty result with `.`
    ///
    /// Validation:
    /// - Rejects any `..` path component (always, regardless of mode)
    /// - Rejects leading `/` when `relative_paths` is false
    /// - Strips leading slashes when `relative_paths` is true
    ///
    /// # Upstream Reference
    ///
    /// - `util1.c:943`: `clean_fname()` with `CFN_REFUSE_DOT_DOT_DIRS`
    /// - `flist.c:756-760`: pathname safety check after `clean_fname`
    fn clean_and_validate_name(&self, name: Vec<u8>) -> io::Result<Vec<u8>> {
        if name.is_empty() {
            return Ok(name);
        }

        // Fast path: most names from a well-behaved sender need no cleaning.
        if !needs_cleaning(&name) {
            if !self.relative_paths && name[0] == b'/' {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "ABORTING due to unsafe pathname from sender: {}",
                        String::from_utf8_lossy(&name)
                    ),
                ));
            }
            return Ok(name);
        }

        // Slow path: normalize and validate.
        let mut out = Vec::with_capacity(name.len());
        let anchored = name[0] == b'/';

        // upstream: flist.c:757 - reject absolute paths when not --relative
        if anchored && !self.relative_paths {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "ABORTING due to unsafe pathname from sender: {}",
                    String::from_utf8_lossy(&name)
                ),
            ));
        }

        // Skip all leading slashes for --relative mode.
        // Non-relative absolute paths were rejected above.
        let start = if anchored {
            name.iter().position(|&b| b != b'/').unwrap_or(name.len())
        } else {
            0
        };

        let mut i = start;
        while i < name.len() {
            // Skip duplicate slashes
            if name[i] == b'/' {
                i += 1;
                continue;
            }

            // Check for `.` or `..` components
            if name[i] == b'.' {
                let next = name.get(i + 1).copied();
                // Single `.` component: skip it
                if next == Some(b'/') || next.is_none() {
                    i += if next.is_some() { 2 } else { 1 };
                    continue;
                }
                // `..` component: always reject
                // upstream: util1.c:982-985 CFN_REFUSE_DOT_DOT_DIRS
                if next == Some(b'.') {
                    let after = name.get(i + 2).copied();
                    if after == Some(b'/') || after.is_none() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "ABORTING due to unsafe pathname from sender: {}",
                                String::from_utf8_lossy(&name)
                            ),
                        ));
                    }
                }
            }

            // Copy this path component
            if !out.is_empty() {
                out.push(b'/');
            }
            while i < name.len() && name[i] != b'/' {
                out.push(name[i]);
                i += 1;
            }
            if i < name.len() {
                i += 1; // skip the slash
            }
        }

        // upstream: util1.c:1004-1005 - empty result becomes "."
        if out.is_empty() {
            out.push(b'.');
        }

        Ok(out)
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
                // upstream: flist.c:recv_file_list() does `io_error |= err`
                // and breaks the loop - it does NOT abort the transfer.
                self.io_error |= code;
                return Ok(None);
            }
            FlagsResult::Flags(f) => f,
        };

        // Step 2: Read name with compression
        let name = self.read_name(reader, flags)?;

        // Step 2b: Reject zero-length filenames.
        // upstream: flist.c:1873 — sender rejects empty names with "cannot send file
        // with empty name". We enforce the same invariant on the receiver side as
        // defense-in-depth against a malicious or buggy sender.
        if name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "received file entry with zero-length filename",
            ));
        }

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
                        atime_nsec: 0,
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

        // Step 10: Apply encoding conversion
        let converted_name = self.apply_encoding_conversion(name)?;

        // Step 8b: Clean and validate the filename.
        // upstream: flist.c:756-760 - clean_fname(CFN_REFUSE_DOT_DOT_DIRS)
        // then reject leading '/' when !relative_paths.
        // In --relative mode, leading slashes are stripped instead.
        let cleaned_name = self.clean_and_validate_name(converted_name)?;

        // Step 9: Construct entry from raw bytes (avoids UTF-8 validation on Unix)
        let mut entry = FileEntry::from_raw_bytes(
            cleaned_name,
            size,
            metadata.mode,
            metadata.mtime,
            metadata.nsec,
            flags,
        );

        // Step 9b: Intern the dirname so entries in the same directory share
        // a single Arc<Path> allocation instead of each holding a separate copy.
        // This mirrors upstream rsync's shared dirname pointer pool.
        let parent = entry.path().parent().filter(|p| !p.as_os_str().is_empty());
        let interned_dirname = match parent {
            Some(p) => self.dirname_interner.intern(p),
            None => self.dirname_interner.intern(Path::new("")),
        };
        entry.set_dirname(interned_dirname);

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
            entry.set_atime_nsec(metadata.atime_nsec);
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

        // Step 22: Read ACLs from the wire (after checksum, before xattrs).
        // upstream: flist.c:1205-1207 - ACLs are read for all non-symlink entries,
        // including abbreviated hardlink followers. Symlinks never carry ACLs.
        if self.preserve_acls && !entry.is_symlink() {
            let (access_ndx, def_ndx) =
                receive_acl_cached(reader, entry.is_dir(), &mut self.acl_cache)?;
            entry.set_acl_ndx(access_ndx);
            if let Some(ndx) = def_ndx {
                entry.set_def_acl_ndx(ndx);
            }
        }

        // Step 23: Read xattr index/data from wire (after ACLs).
        // upstream: flist.c:1209-1212 - receive_xattr() is called after
        // receive_acl() and runs for ALL entries including hardlink followers.
        if self.preserve_xattrs {
            let xattr_ndx = self.xattr_cache.receive_xattr(reader)?;
            entry.set_xattr_ndx(xattr_ndx);
        }

        // Step 24: Update statistics
        self.update_stats(&entry);

        // Level 2: Individual file entry
        debug_log!(
            Flist,
            2,
            "recv_file_entry: {:?} size={} mode={:o}",
            entry.name(),
            entry.size(),
            entry.mode()
        );

        // Level 3: Encoding/decoding details
        debug_log!(
            Flist,
            3,
            "recv_file_entry details: mtime={} uid={:?} gid={:?} flags={:#x}",
            entry.mtime(),
            entry.uid(),
            entry.gid(),
            flags.primary as u32 | ((flags.extended as u32) << 8)
        );

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

        // Level 4: Wire format bytes for name compression
        debug_log!(
            Flist,
            4,
            "read_name: same_len={} suffix_len={} long_name={}",
            same_len,
            suffix_len,
            flags.long_name()
        );

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

        // Level 3: Encoding/decoding details for name
        debug_log!(
            Flist,
            3,
            "read_name: total_len={} name_bytes={:?}",
            name.len(),
            &name[..name.len().min(64)]
        );

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
        // Level 4: Wire format bytes for file size
        debug_log!(Flist, 4, "read_size: size={}", size);
        Ok(size as u64)
    }
}

/// Reads a single file entry from a reader.
///
/// Reads an owner ID (uid or gid) and optional name from the wire.
///
/// Returns `(id, optional_name)`. When `same` is true, returns the previous
/// value unchanged. Otherwise reads the ID using fixed or varint encoding,
/// and optionally reads a name string if `name_follows` is set.
fn read_owner_id<R: Read + ?Sized>(
    reader: &mut R,
    same: bool,
    name_follows: bool,
    prev_id: u32,
    fixed_encoding: bool,
) -> io::Result<(u32, Option<String>)> {
    if same {
        return Ok((prev_id, None));
    }

    let id = if fixed_encoding {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        i32::from_le_bytes(buf) as u32
    } else {
        read_varint(reader)? as u32
    };

    let name = if name_follows {
        let mut len_buf = [0u8; 1];
        reader.read_exact(&mut len_buf)?;
        let len = len_buf[0] as usize;
        if len > 0 {
            let mut name_bytes = vec![0u8; len];
            reader.read_exact(&mut name_bytes)?;
            Some(match String::from_utf8(name_bytes) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            })
        } else {
            None
        }
    } else {
        None
    };

    Ok((id, name))
}

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

/// Returns true if a filename needs normalization or contains unsafe components.
///
/// Checks for patterns that `clean_and_validate_name` would modify:
/// leading slashes, duplicate slashes, `.` or `..` path components.
/// This allows a fast bypass for the common case of well-formed names.
fn needs_cleaning(name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }

    // Leading slash requires stripping or rejection
    if name[0] == b'/' {
        return true;
    }

    let mut i = 0;
    while i < name.len() {
        // Duplicate slashes
        if name[i] == b'/' {
            if i + 1 < name.len() && name[i + 1] == b'/' {
                return true;
            }
            i += 1;
            continue;
        }

        // Check for `.` or `..` at component start
        if name[i] == b'.' {
            let at_start = i == 0 || name[i - 1] == b'/';
            if at_start {
                let next = name.get(i + 1).copied();
                // "." component
                if next == Some(b'/') || next.is_none() {
                    return true;
                }
                // ".." component
                if next == Some(b'.') {
                    let after = name.get(i + 2).copied();
                    if after == Some(b'/') || after.is_none() {
                        return true;
                    }
                }
            }
        }

        i += 1;
    }

    // Trailing slash
    if name[name.len() - 1] == b'/' {
        return true;
    }

    false
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

        // io_error markers are now accumulated (upstream: flist.c io_error |= err)
        // rather than returned as hard errors.
        assert!(result.unwrap().is_none());
        assert_eq!(reader.io_error(), 42);
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

        assert!(result.unwrap().is_none());
        assert_eq!(reader.io_error(), 99);
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

        assert!(result.unwrap().is_none());
        assert_eq!(reader.io_error(), 123);
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

        // Test end marker with non-zero error accumulates io_error
        let mut data2 = Vec::new();
        writer.write_end(&mut data2, Some(123)).unwrap();

        let mut reader2 = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor2 = Cursor::new(&data2[..]);
        let result2 = reader2.read_entry(&mut cursor2);
        assert!(result2.unwrap().is_none());
        assert_eq!(reader2.io_error(), 123);
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
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        // Block device with small minor (fits in byte)
        let mut entry = FileEntry::new_block_device("dev/sda".into(), 0o644, 8, 0);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

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
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        // Block device with large minor (needs 4-byte int)
        let mut entry = FileEntry::new_block_device("dev/nvme0n1".into(), 0o644, 259, 65536);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

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
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        // Two devices with same major - tests XMIT_SAME_RDEV_MAJOR flag
        let mut entry1 = FileEntry::new_block_device("dev/sda1".into(), 0o644, 8, 1);
        entry1.set_mtime(1700000000, 0);

        let mut entry2 = FileEntry::new_block_device("dev/sda2".into(), 0o644, 8, 2);
        entry2.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry1).unwrap();
        writer.write_entry(&mut data, &entry2).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

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
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let mut entry = FileEntry::new_block_device("dev/loop0".into(), 0o644, 7, 12345);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

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
        .with_preserve_devices(true)
        .with_preserve_specials(true);

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
        .with_preserve_devices(true)
        .with_preserve_specials(true);

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
        let mut reader =
            FileListReader::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);

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
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        // Block device with large minor (needs 4-byte int, not 1-byte)
        let mut entry = FileEntry::new_block_device("dev/nvme0n1".into(), 0o644, 259, 65536);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        // Truncate the last 2 bytes (partial 4-byte minor)
        let truncated_data = &data[..data.len() - 2];

        let mut cursor = Cursor::new(truncated_data);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let result = reader.read_entry(&mut cursor);
        assert_unexpected_eof(result, "truncated protocol 29 device minor (int)");
    }

    /// Test reading a 3GB file entry (above 2^31 = 2GB boundary).
    /// Verifies the reader correctly decodes varlong-encoded large file sizes.
    #[test]
    fn read_large_file_size_3gb() {
        use super::super::write::FileListWriter;

        const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024; // 3 * 1024^3 = 3,221,225,472 bytes

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("huge_3gb.dat".into(), SIZE_3GB, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "huge_3gb.dat");
        assert_eq!(
            read_entry.size(),
            SIZE_3GB,
            "Reader should correctly decode 3GB file size (above 2^31 boundary)"
        );
    }

    /// Test reading a 5GB file entry (above 2^32 = 4GB boundary).
    /// Verifies the reader correctly decodes varlong-encoded very large file sizes.
    #[test]
    fn read_large_file_size_5gb() {
        use super::super::write::FileListWriter;

        const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024; // 5 * 1024^3 = 5,368,709,120 bytes

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("huge_5gb.dat".into(), SIZE_5GB, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "huge_5gb.dat");
        assert_eq!(
            read_entry.size(),
            SIZE_5GB,
            "Reader should correctly decode 5GB file size (above 2^32 boundary)"
        );
    }

    /// Test reading file entries at critical size boundaries (2^31, 2^32).
    /// These boundaries are important because they represent the limits of
    /// 32-bit signed and unsigned integer ranges.
    #[test]
    fn read_large_file_sizes_at_boundaries() {
        use super::super::write::FileListWriter;

        // Critical boundary values
        let boundary_sizes: &[(u64, &str)] = &[
            ((1u64 << 31) - 1, "max_i32"), // 2,147,483,647 (max signed 32-bit)
            (1u64 << 31, "2gb"),           // 2,147,483,648 (2GB exactly)
            ((1u64 << 31) + 1, "2gb_plus_1"),
            ((1u64 << 32) - 1, "max_u32"), // 4,294,967,295 (max unsigned 32-bit)
            (1u64 << 32, "4gb"),           // 4,294,967,296 (4GB exactly)
            ((1u64 << 32) + 1, "4gb_plus_1"),
        ];

        let protocol = test_protocol();

        for (size, label) in boundary_sizes {
            let mut data = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let filename = format!("boundary_{label}.bin");
            let mut entry = FileEntry::new_file(filename.clone().into(), *size, 0o100644);
            entry.set_mtime(1700000000, 0);

            writer.write_entry(&mut data, &entry).unwrap();

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.name(), &filename);
            assert_eq!(
                read_entry.size(),
                *size,
                "Reader should correctly decode size {size} at {label} boundary"
            );
        }
    }

    // =========================================================================
    // Zero-length filename validation tests
    //
    // upstream: flist.c:1873 — sender rejects empty names. These tests verify
    // that the receiver also rejects zero-length filenames as defense-in-depth.
    // =========================================================================

    #[test]
    fn read_entry_rejects_zero_length_filename() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        // Craft wire data with a zero-length filename:
        // flags=0x01 (valid, not end-of-list), suffix_len=0, no XMIT_SAME_NAME
        let mut data = Vec::new();
        encode_varint_to_vec(0x01, &mut data); // Valid flags
        data.push(0u8); // suffix_len = 0 (zero-length filename)

        // The name check fires immediately after read_name, before any further
        // wire reads (hardlink index, size, metadata), so no additional data
        // is needed in the stream.

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let result = reader.read_entry(&mut cursor);
        assert!(result.is_err(), "zero-length filename should be rejected");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("zero-length filename"),
            "error message should mention zero-length filename, got: {err}"
        );
    }

    #[test]
    fn read_entry_accepts_non_empty_filename() {
        use super::super::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("a".into(), 1, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "a");
    }

    #[test]
    fn read_entry_rejects_zero_length_filename_nonvarint() {
        // Non-varint mode (default): flags are a single byte.
        // Flags byte 0x01 (valid) + suffix_len=0 -> empty filename.
        let data: &[u8] = &[
            0x01, // flags byte (valid, not end-of-list, no XMIT_SAME_NAME)
            0x00, // suffix_len = 0 (zero-length filename)
        ];

        let mut cursor = Cursor::new(data);
        let mut reader = FileListReader::new(test_protocol());

        let result = reader.read_entry(&mut cursor);
        assert!(result.is_err(), "zero-length filename should be rejected");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("zero-length filename"));
    }

    /// Test reading large file sizes with legacy protocol 29 (longint encoding).
    /// Protocol 29 uses a different encoding: 4 bytes for small values,
    /// 12 bytes (marker + 8 bytes) for values > 0x7FFFFFFF.
    #[test]
    fn read_large_file_size_legacy_protocol() {
        use super::super::write::FileListWriter;

        const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;
        const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

        let protocol = ProtocolVersion::try_from(29u8).unwrap();

        for (size, label) in [(SIZE_3GB, "3GB"), (SIZE_5GB, "5GB")] {
            let mut data = Vec::new();
            let mut writer = FileListWriter::new(protocol);

            let mut entry = FileEntry::new_file("legacy_large.bin".into(), size, 0o100644);
            entry.set_mtime(1700000000, 0);

            writer.write_entry(&mut data, &entry).unwrap();

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(
                read_entry.size(),
                size,
                "Legacy protocol reader should correctly decode {label} file size"
            );
        }
    }

    /// Verifies that non-varint mode consumes exactly one zero byte for end-of-list.
    ///
    /// Upstream: `flist.c:recv_file_list()` — without `xfer_flags_as_varint`,
    /// `read_byte(f)` returns 0 and the loop breaks immediately.
    #[test]
    fn read_end_of_list_nonvarint_consumes_single_byte() {
        let reader = FileListReader::new(test_protocol());
        // One zero byte followed by a sentinel
        let data = [0x00, 0xFF];
        let mut cursor = Cursor::new(&data[..]);

        let result = reader.read_flags(&mut cursor).unwrap();
        assert!(matches!(result, FlagsResult::EndOfList));
        assert_eq!(
            cursor.position(),
            1,
            "non-varint end-of-list must consume exactly 1 byte"
        );
    }

    /// Verifies that varint mode consumes exactly two zero bytes for end-of-list
    /// (varint(0) for flags + varint(0) for error code).
    ///
    /// Upstream: `flist.c:recv_file_list()` — with `xfer_flags_as_varint`,
    /// `read_varint(f)` returns 0 for flags, then `read_varint(f)` returns 0 for error.
    #[test]
    fn read_end_of_list_varint_consumes_two_bytes() {
        let reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );
        // Two zero bytes (varint(0) + varint(0)), followed by a sentinel
        let data = [0x00, 0x00, 0xFF];
        let mut cursor = Cursor::new(&data[..]);

        let result = reader.read_flags(&mut cursor).unwrap();
        assert!(matches!(result, FlagsResult::EndOfList));
        assert_eq!(
            cursor.position(),
            2,
            "varint end-of-list must consume exactly 2 bytes (flags=0 + error=0)"
        );
    }

    /// Verifies that varint mode with non-zero error code consumes the second
    /// varint and returns IoError.
    ///
    /// Upstream: `flist.c:recv_file_list()` — flags varint = 0, error varint != 0
    /// causes `io_error |= err`.
    #[test]
    fn read_end_of_list_varint_with_error_returns_io_error() {
        use crate::varint::encode_varint_to_vec;

        let reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        let mut data = Vec::new();
        encode_varint_to_vec(0, &mut data); // flags = 0
        encode_varint_to_vec(7, &mut data); // error = 7
        data.push(0xFF); // sentinel (should not be consumed)

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_flags(&mut cursor).unwrap();

        match result {
            FlagsResult::IoError(code) => assert_eq!(code, 7),
            other => panic!("expected IoError(7), got {other:?}"),
        }
        assert_eq!(
            cursor.position() as usize,
            data.len() - 1,
            "must consume flags varint + error varint but not the sentinel"
        );
    }

    /// Verifies round-trip: varint write_end -> read_flags produces EndOfList
    /// and consumes exactly the written bytes.
    #[test]
    fn read_write_end_of_list_varint_round_trip_exact_bytes() {
        use super::super::write::FileListWriter;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut buf = Vec::new();
        writer.write_end(&mut buf, None).unwrap();

        // Must produce exactly [0x00, 0x00]
        assert_eq!(buf, [0x00, 0x00], "varint end marker without error");

        let reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&buf[..]);
        let result = reader.read_flags(&mut cursor).unwrap();

        assert!(matches!(result, FlagsResult::EndOfList));
        assert_eq!(
            cursor.position() as usize,
            buf.len(),
            "must consume all written bytes"
        );
    }

    /// Verifies round-trip: non-varint write_end -> read_flags produces EndOfList
    /// and consumes exactly one byte.
    #[test]
    fn read_write_end_of_list_nonvarint_round_trip_exact_bytes() {
        use super::super::write::FileListWriter;

        let protocol = test_protocol();

        let writer = FileListWriter::new(protocol);
        let mut buf = Vec::new();
        writer.write_end(&mut buf, None).unwrap();

        // Must produce exactly [0x00]
        assert_eq!(buf, [0x00], "non-varint end marker");

        let reader = FileListReader::new(protocol);
        let mut cursor = Cursor::new(&buf[..]);
        let result = reader.read_flags(&mut cursor).unwrap();

        assert!(matches!(result, FlagsResult::EndOfList));
        assert_eq!(
            cursor.position() as usize,
            buf.len(),
            "must consume all written bytes"
        );
    }

    /// Tests for ACL integration in the flist read path.
    mod acl_integration {
        use super::*;
        use crate::acl::{AclCache, AclType, RsyncAcl, send_acl, send_rsync_acl};

        /// Writes a file entry followed by ACL data, then reads it back
        /// with `preserve_acls` enabled.
        #[test]
        fn read_entry_with_access_acl() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            // Write file entry
            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("test_acl.txt".into(), 200, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // Write ACL data (as sender would after send_file_entry)
            // upstream: flist.c send_acl() is called after send_file_entry()
            let mut acl = RsyncAcl::new();
            acl.user_obj = 0x06; // rw-
            acl.group_obj = 0x04; // r--
            acl.other_obj = 0x04; // r--
            let mut acl_cache = AclCache::new();
            send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();

            // Read it back with preserve_acls
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.name(), "test_acl.txt");
            assert_eq!(read_entry.size(), 200);
            assert_eq!(read_entry.acl_ndx(), Some(0));
            assert!(read_entry.def_acl_ndx().is_none());

            // Verify cached ACL matches what was sent
            let cached = reader.acl_cache().get_access(0).unwrap();
            assert_eq!(cached.user_obj, 0x06);
            assert_eq!(cached.group_obj, 0x04);
            assert_eq!(cached.other_obj, 0x04);

            // All bytes consumed
            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Writes a directory entry with access + default ACL, reads it back.
        #[test]
        fn read_directory_entry_with_access_and_default_acl() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            // Write directory entry
            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // Write access + default ACLs
            let access_acl = {
                let mut a = RsyncAcl::new();
                a.user_obj = 0x07;
                a.group_obj = 0x05;
                a.other_obj = 0x05;
                a
            };
            let default_acl = {
                let mut a = RsyncAcl::new();
                a.user_obj = 0x07;
                a.group_obj = 0x05;
                a.other_obj = 0x00;
                a
            };
            let mut acl_cache = AclCache::new();
            send_acl(
                &mut data,
                &access_acl,
                Some(&default_acl),
                true,
                &mut acl_cache,
            )
            .unwrap();

            // Read it back
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.name(), "mydir");
            assert!(read_entry.is_dir());
            assert_eq!(read_entry.acl_ndx(), Some(0));
            assert_eq!(read_entry.def_acl_ndx(), Some(0));

            let cached_access = reader.acl_cache().get_access(0).unwrap();
            assert_eq!(cached_access.user_obj, 0x07);
            assert_eq!(cached_access.other_obj, 0x05);

            let cached_default = reader.acl_cache().get_default(0).unwrap();
            assert_eq!(cached_default.user_obj, 0x07);
            assert_eq!(cached_default.other_obj, 0x00);

            assert_eq!(cursor.position() as usize, data.len());
        }

        /// ACLs are NOT read for symlink entries (matching upstream behavior).
        #[test]
        fn read_symlink_entry_skips_acl() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            // Write symlink entry
            let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
            let mut entry = FileEntry::new_symlink("link".into(), "target".into());
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // No ACL data follows for symlinks (sender doesn't send it)

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_acls(true)
                .with_preserve_links(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert!(read_entry.is_symlink());
            assert!(read_entry.acl_ndx().is_none());

            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Without preserve_acls, no ACL reading occurs even if data follows.
        #[test]
        fn read_entry_without_preserve_acls_skips_acl() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // preserve_acls is false, so reader should NOT try to read ACL data
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert!(read_entry.acl_ndx().is_none());
            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Multiple entries share cached ACLs correctly.
        #[test]
        fn multiple_entries_share_cached_acls() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();
            let mut acl_cache = AclCache::new();

            let acl = {
                let mut a = RsyncAcl::new();
                a.user_obj = 0x07;
                a.group_obj = 0x05;
                a.other_obj = 0x04;
                a
            };

            // Write two file entries with the same ACL
            let mut writer = FileListWriter::new(protocol);
            for name in &["file1.txt", "file2.txt"] {
                let mut entry = FileEntry::new_file((*name).into(), 100, 0o100644);
                entry.set_mtime(1700000000, 0);
                writer.write_entry(&mut data, &entry).unwrap();
                send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();
            }

            // Read them back
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

            let entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(entry1.name(), "file1.txt");
            assert_eq!(entry1.acl_ndx(), Some(0));
            assert_eq!(entry2.name(), "file2.txt");
            // Second entry gets cache hit - same index
            assert_eq!(entry2.acl_ndx(), Some(0));

            // Only one ACL in cache
            assert_eq!(reader.acl_cache().access_count(), 1);
        }
    }

    /// Tests for xattr integration in the flist read path.
    mod xattr_integration {
        use super::*;
        use crate::varint::write_varint;

        /// Helper to append literal xattr data to a buffer in wire format.
        /// Each entry is (name_bytes, value_bytes).
        fn write_literal_xattr(buf: &mut Vec<u8>, entries: &[(&[u8], &[u8])]) {
            // ndx = 0 means literal follows
            write_varint(buf, 0).unwrap();
            // count
            write_varint(buf, entries.len() as i32).unwrap();
            for &(name, value) in entries {
                // name_len includes NUL terminator
                write_varint(buf, (name.len() + 1) as i32).unwrap();
                // datum_len
                write_varint(buf, value.len() as i32).unwrap();
                // name bytes + NUL
                buf.extend_from_slice(name);
                buf.push(0);
                // value
                buf.extend_from_slice(value);
            }
        }

        /// Helper to append a cache-hit xattr reference.
        fn write_xattr_cache_hit(buf: &mut Vec<u8>, index: u32) {
            // ndx = index + 1 (non-zero means cache hit)
            write_varint(buf, (index + 1) as i32).unwrap();
        }

        /// Reads a file entry with xattr data and verifies the xattr index
        /// and cached xattr list.
        #[test]
        fn read_entry_with_xattr() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            // Write file entry
            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("test_xattr.txt".into(), 300, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // Append literal xattr data (as sender would after send_file_entry)
            write_literal_xattr(
                &mut data,
                &[(b"user.mime_type", b"text/plain"), (b"user.tag", b"test")],
            );

            // Read it back with preserve_xattrs
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.name(), "test_xattr.txt");
            assert_eq!(read_entry.size(), 300);
            assert_eq!(read_entry.xattr_ndx(), Some(0));

            // Verify cached xattr list
            let cached = reader.xattr_cache().get(0).unwrap();
            assert_eq!(cached.len(), 2);
            assert_eq!(cached.entries()[0].name(), b"user.mime_type");
            assert_eq!(cached.entries()[0].datum(), b"text/plain");
            assert_eq!(cached.entries()[1].name(), b"user.tag");
            assert_eq!(cached.entries()[1].datum(), b"test");

            // All bytes consumed
            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Without preserve_xattrs, no xattr reading occurs.
        #[test]
        fn read_entry_without_preserve_xattrs_skips_xattr() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // preserve_xattrs is false, so reader should NOT try to read xattr data
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert!(read_entry.xattr_ndx().is_none());
            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Multiple entries reference cached xattr sets correctly.
        #[test]
        fn multiple_entries_share_cached_xattrs() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            // Write first file entry + literal xattr
            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            write_literal_xattr(&mut data, &[(b"user.attr", b"value")]);

            // Write second file entry + cache hit referencing index 0
            let mut entry = FileEntry::new_file("file2.txt".into(), 200, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            write_xattr_cache_hit(&mut data, 0);

            // Read them back
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

            let entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(entry1.name(), "file1.txt");
            assert_eq!(entry1.xattr_ndx(), Some(0));
            assert_eq!(entry2.name(), "file2.txt");
            assert_eq!(entry2.xattr_ndx(), Some(0));

            // Only one xattr set in cache
            assert_eq!(reader.xattr_cache().len(), 1);

            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Directory entries receive xattr data just like files.
        #[test]
        fn read_directory_entry_with_xattr() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            write_literal_xattr(
                &mut data,
                &[(b"security.selinux", b"system_u:object_r:default_t:s0")],
            );

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert!(read_entry.is_dir());
            assert_eq!(read_entry.xattr_ndx(), Some(0));

            let cached = reader.xattr_cache().get(0).unwrap();
            assert_eq!(cached.len(), 1);
            assert_eq!(cached.entries()[0].name(), b"security.selinux");

            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Symlink entries also receive xattr data (unlike ACLs, xattrs apply
        /// to all file types). Upstream: xattrs.c does not exclude symlinks.
        #[test]
        fn read_symlink_entry_with_xattr() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
            let mut entry = FileEntry::new_symlink("link".into(), "target".into());
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            write_literal_xattr(&mut data, &[(b"user.symattr", b"symval")]);

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_xattrs(true)
                .with_preserve_links(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert!(read_entry.is_symlink());
            assert_eq!(read_entry.xattr_ndx(), Some(0));

            assert_eq!(cursor.position() as usize, data.len());
        }

        /// When both ACLs and xattrs are enabled, ACLs are read first
        /// then xattrs, matching upstream wire order.
        #[test]
        fn read_entry_with_acl_and_xattr() {
            use super::super::super::write::FileListWriter;
            use crate::acl::{AclCache, AclType, RsyncAcl, send_rsync_acl};

            let protocol = test_protocol();
            let mut data = Vec::new();

            // Write file entry
            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("both.txt".into(), 150, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // Write ACL data first (upstream order)
            let mut acl = RsyncAcl::new();
            acl.user_obj = 0x06;
            acl.group_obj = 0x04;
            acl.other_obj = 0x04;
            let mut acl_cache = AclCache::new();
            send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();

            // Then write xattr data (after ACL on wire)
            write_literal_xattr(&mut data, &[(b"user.key", b"val")]);

            // Read it back with both enabled
            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_acls(true)
                .with_preserve_xattrs(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.name(), "both.txt");
            assert_eq!(read_entry.acl_ndx(), Some(0));
            assert_eq!(read_entry.xattr_ndx(), Some(0));

            // Verify ACL cache
            let cached_acl = reader.acl_cache().get_access(0).unwrap();
            assert_eq!(cached_acl.user_obj, 0x06);

            // Verify xattr cache
            let cached_xattr = reader.xattr_cache().get(0).unwrap();
            assert_eq!(cached_xattr.len(), 1);
            assert_eq!(cached_xattr.entries()[0].name(), b"user.key");
            assert_eq!(cached_xattr.entries()[0].datum(), b"val");

            // All bytes consumed
            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Empty xattr set is stored in cache with index.
        #[test]
        fn read_entry_with_empty_xattr_set() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            let mut writer = FileListWriter::new(protocol);
            let mut entry = FileEntry::new_file("empty_xattr.txt".into(), 50, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();

            // Write empty literal xattr set
            write_literal_xattr(&mut data, &[]);

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

            let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
            assert_eq!(read_entry.xattr_ndx(), Some(0));

            let cached = reader.xattr_cache().get(0).unwrap();
            assert!(cached.is_empty());

            assert_eq!(cursor.position() as usize, data.len());
        }

        /// Multiple different xattr sets get distinct cache indices.
        #[test]
        fn multiple_distinct_xattr_sets() {
            use super::super::super::write::FileListWriter;

            let protocol = test_protocol();
            let mut data = Vec::new();

            let mut writer = FileListWriter::new(protocol);

            // First entry with one xattr set
            let mut entry = FileEntry::new_file("a.txt".into(), 100, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            write_literal_xattr(&mut data, &[(b"user.color", b"red")]);

            // Second entry with a different xattr set
            let mut entry = FileEntry::new_file("b.txt".into(), 200, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            write_literal_xattr(&mut data, &[(b"user.color", b"blue")]);

            // Third entry referencing the first set
            let mut entry = FileEntry::new_file("c.txt".into(), 300, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            write_xattr_cache_hit(&mut data, 0);

            let mut cursor = Cursor::new(&data[..]);
            let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

            let e1 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let e2 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let e3 = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(e1.xattr_ndx(), Some(0));
            assert_eq!(e2.xattr_ndx(), Some(1));
            assert_eq!(e3.xattr_ndx(), Some(0)); // cache hit

            assert_eq!(reader.xattr_cache().len(), 2);

            let first = reader.xattr_cache().get(0).unwrap();
            assert_eq!(first.entries()[0].datum(), b"red");

            let second = reader.xattr_cache().get(1).unwrap();
            assert_eq!(second.entries()[0].datum(), b"blue");

            assert_eq!(cursor.position() as usize, data.len());
        }
    }
}
