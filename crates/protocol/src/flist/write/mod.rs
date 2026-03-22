//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver. The writer maintains compression
//! state to omit fields that match the previous entry, reducing wire traffic.
//!
//! # Upstream Reference
//!
//! See `flist.c:send_file_entry()` for the canonical wire format encoding.

mod encoding;
mod metadata;
mod xflags;

#[cfg(test)]
mod tests;

use std::io::{self, Write};

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::codec::{ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;

use super::entry::FileEntry;
use super::state::{FileListCompressionState, FileListStats};

/// Boolean flags controlling which file attributes are preserved on the wire.
///
/// Groups the `preserve_*` options that determine whether specific metadata
/// fields (UID, GID, symlinks, devices, etc.) are encoded in the file list.
/// Used by both [`FileListWriter`] and [`BatchedFileListWriter`] to configure
/// which fields appear in the wire format.
///
/// These correspond to the `--owner`, `--group`, `--links`, `--devices`,
/// `--specials`, `--hard-links`, `--atimes`, `--crtimes`, `--acls`, and
/// `--xattrs` command-line options negotiated during protocol setup.
///
/// [`BatchedFileListWriter`]: super::batched_writer::BatchedFileListWriter
#[derive(Debug, Clone, Copy, Default)]
pub struct PreserveFlags {
    /// Whether to preserve (and thus write) UID values to the wire.
    pub uid: bool,
    /// Whether to preserve (and thus write) GID values to the wire.
    pub gid: bool,
    /// Whether to preserve (and thus write) symlink targets to the wire.
    pub links: bool,
    /// Whether to preserve (and thus write) device numbers (block/char) to the wire.
    pub devices: bool,
    /// Whether to preserve (and thus write) special files (FIFOs/sockets) to the wire.
    pub specials: bool,
    /// Whether to preserve (and thus write) hardlink indices to the wire.
    pub hard_links: bool,
    /// Whether to preserve (and thus write) access times to the wire.
    pub atimes: bool,
    /// Whether to preserve (and thus write) creation times to the wire.
    pub crtimes: bool,
    /// Whether to preserve (and thus write) ACLs to the wire.
    pub acls: bool,
    /// Whether to preserve (and thus write) extended attributes to the wire.
    pub xattrs: bool,
}

/// State maintained while writing a file list to the wire.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This writer maintains the necessary state
/// to encode these compressed entries.
///
/// # Upstream Reference
///
/// Mirrors the static local state in `flist.c:send_file_entry()` - the
/// `lastname`, `modtime`, `mode`, `uid`, `gid`, `rdev`, and `rdev_major`
/// variables that persist across calls during `send_file_list()`.
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
    /// Flags controlling which file attributes are preserved on the wire.
    preserve: PreserveFlags,
    /// Whether to send checksums for all files (--checksum / -c mode).
    always_checksum: bool,
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
            preserve: PreserveFlags::default(),
            always_checksum: false,
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
            preserve: PreserveFlags::default(),
            always_checksum: false,
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
        self.preserve.uid = preserve;
        self
    }

    /// Sets whether GID values should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.preserve.gid = preserve;
        self
    }

    /// Sets whether symlink targets should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_links(mut self, preserve: bool) -> Self {
        self.preserve.links = preserve;
        self
    }

    /// Sets whether device numbers (block/char) should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.preserve.devices = preserve;
        self
    }

    /// Sets whether special files (FIFOs/sockets) should be written to the wire.
    ///
    /// # Upstream Reference
    ///
    /// Upstream `flist.c:send_file_entry()` checks `preserve_specials` separately
    /// from `preserve_devices` for `IS_SPECIAL()` file types.
    #[inline]
    #[must_use]
    pub const fn with_preserve_specials(mut self, preserve: bool) -> Self {
        self.preserve.specials = preserve;
        self
    }

    /// Sets whether hardlink indices should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_hard_links(mut self, preserve: bool) -> Self {
        self.preserve.hard_links = preserve;
        self
    }

    /// Sets whether access times should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_atimes(mut self, preserve: bool) -> Self {
        self.preserve.atimes = preserve;
        self
    }

    /// Sets whether creation times should be written to the wire.
    #[inline]
    #[must_use]
    pub const fn with_preserve_crtimes(mut self, preserve: bool) -> Self {
        self.preserve.crtimes = preserve;
        self
    }

    /// Sets whether ACLs should be written to the wire.
    ///
    /// When enabled, ACL indices are written after other metadata.
    /// Note: ACL data itself is sent in a separate exchange.
    #[inline]
    #[must_use]
    pub const fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.preserve.acls = preserve;
        self
    }

    /// Sets whether extended attributes should be written to the wire.
    ///
    /// When enabled, xattr indices are written after ACL indices.
    /// Note: Xattr data itself is sent in a separate exchange.
    #[inline]
    #[must_use]
    pub const fn with_preserve_xattrs(mut self, preserve: bool) -> Self {
        self.preserve.xattrs = preserve;
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
}

/// Writes a single file entry to a writer.
///
/// Convenience function for writing individual entries without maintaining
/// writer state. For writing multiple entries, use [`FileListWriter`] to
/// benefit from cross-entry compression.
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` for the canonical wire format encoding.
pub fn write_file_entry<W: Write>(
    writer: &mut W,
    entry: &FileEntry,
    protocol: ProtocolVersion,
) -> io::Result<()> {
    let mut list_writer = FileListWriter::new(protocol);
    list_writer.write_entry(writer, entry)
}
