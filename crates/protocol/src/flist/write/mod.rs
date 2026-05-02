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
use crate::acl::{AclCache, RsyncAcl, send_acl};
use crate::codec::{ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::xattr::{XattrCache, XattrList, send_xattr};

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
    /// Wire NDX start of the current flist segment.
    ///
    /// Used to distinguish abbreviated vs unabbreviated hardlink followers.
    /// Abbreviated followers (leader in same segment, idx >= first_ndx) have
    /// metadata skipped on wire. Unabbreviated followers carry full metadata.
    /// upstream: flist.c:send_file_entry() line 572
    first_ndx: i32,
    /// ACL cache for deduplication across entries.
    /// upstream: acls.c - sender maintains cache of sent ACLs.
    acl_cache: AclCache,
    /// ACL data pending for the next `write_entry` call.
    ///
    /// When set, `write_entry` uses this instead of faking ACL from mode.
    /// The caller (generator) reads real filesystem ACLs and sets this
    /// before each `write_entry`. Reset to `None` after use.
    ///
    /// Tuple: (access_acl, optional_default_acl_for_dirs).
    pending_acl: Option<(RsyncAcl, Option<RsyncAcl>)>,
    /// Xattr cache for sender-side deduplication across entries.
    /// upstream: xattrs.c - `find_matching_xattr()` + `rsync_xal_store()`
    xattr_cache: XattrCache,
    /// Checksum seed for xattr abbreviated value digests.
    /// upstream: xattrs.c - `sum_init(xattr_sum_nni, checksum_seed)`
    checksum_seed: i32,
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
            first_ndx: 0,
            acl_cache: AclCache::new(),
            pending_acl: None,
            xattr_cache: XattrCache::new(),
            checksum_seed: 0,
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
            first_ndx: 0,
            acl_cache: AclCache::new(),
            pending_acl: None,
            xattr_cache: XattrCache::new(),
            checksum_seed: 0,
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
    /// When enabled, ACL data is written after the checksum for each entry.
    #[inline]
    #[must_use]
    pub const fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.preserve.acls = preserve;
        self
    }

    /// Sets the ACL data for the next `write_entry` call.
    ///
    /// The caller reads real filesystem ACLs and provides them here. The writer
    /// strips base permission entries before sending (matching upstream's
    /// `rsync_acl_strip_perms`). The pending ACL is consumed by the next
    /// `write_entry` call and reset to `None`.
    ///
    /// When not set, `write_entry` falls back to `RsyncAcl::from_mode()` for
    /// backward compatibility.
    ///
    /// # Arguments
    ///
    /// * `access_acl` - The file's access ACL
    /// * `default_acl` - The directory's default ACL (pass `None` for non-directories)
    pub fn set_pending_acl(&mut self, access_acl: RsyncAcl, default_acl: Option<RsyncAcl>) {
        self.pending_acl = Some((access_acl, default_acl));
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

    /// Sets the checksum seed for xattr abbreviated value digests.
    ///
    /// upstream: `xattrs.c` - `sum_init(xattr_sum_nni, checksum_seed)`
    #[inline]
    #[must_use]
    pub const fn with_checksum_seed(mut self, seed: i32) -> Self {
        self.checksum_seed = seed;
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

    /// Sets the wire NDX start of the current flist segment.
    ///
    /// Must be called before writing entries in each flist segment so that
    /// abbreviated vs unabbreviated hardlink followers are distinguished
    /// correctly on the wire.
    /// upstream: flist.c:send_file_entry() uses `first_ndx` parameter
    pub fn set_first_ndx(&mut self, first_ndx: i32) {
        self.first_ndx = first_ndx;
    }

    /// Returns true if this is an abbreviated hardlink follower whose metadata
    /// should be skipped on the wire.
    ///
    /// An abbreviated follower has its leader in the SAME flist segment
    /// (`hardlink_idx >= first_ndx`), so metadata is omitted. Unabbreviated
    /// followers (leader in a previous segment) carry full metadata.
    /// upstream: flist.c:send_file_entry() line 572
    #[inline]
    fn is_abbreviated_follower(&self, entry: &FileEntry, xflags: u32) -> bool {
        if !self.is_hardlink_follower(xflags) {
            return false;
        }
        match entry.hardlink_idx() {
            Some(idx) => (idx as i32) >= self.first_ndx,
            None => false,
        }
    }

    /// Writes a file entry to the stream.
    ///
    /// Wire format order (matching upstream rsync flist.c send_file_entry):
    /// 1. Flags
    /// 2. Name (with prefix compression)
    /// 3. Hardlink index (if follower) - then STOP for abbreviated followers
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
    pub fn write_entry<W: Write>(&mut self, writer: &mut W, entry: &FileEntry) -> io::Result<()> {
        let raw_name = entry.name_bytes();
        // upstream: flist.c send_file_entry() iconv_buf(ic_send, ...) - when
        // --iconv is in effect, the filename is transcoded from local to
        // remote charset before any prefix-compression bookkeeping or wire
        // emission, so all downstream xflags and length fields refer to the
        // converted bytes.
        let name = self.apply_encoding_conversion(&raw_name)?;

        let same_len = self.state.calculate_name_prefix_len(&name);
        let suffix_len = name.len() - same_len;
        let xflags = self.calculate_xflags(entry, same_len, suffix_len);

        self.write_flags(writer, xflags, entry.is_dir())?;
        self.write_name(writer, &name, same_len, suffix_len, xflags)?;
        self.write_hardlink_idx(writer, entry, xflags)?;

        // Abbreviated followers (leader in same flist segment) have metadata
        // copied from the leader; unabbreviated followers carry full metadata.
        // upstream: flist.c:send_file_entry() line 572
        let abbreviated = self.is_abbreviated_follower(entry, xflags);
        if !abbreviated {
            self.write_metadata(writer, entry, xflags)?;
            self.write_rdev(writer, entry, xflags)?;
            self.write_symlink_target(writer, entry)?;
            self.write_hardlink_dev_ino(writer, entry, xflags)?;
        }

        // upstream: always_checksum && (S_ISREG(mode) || protocol_version < 28)
        if !abbreviated {
            self.write_checksum(writer, entry)?;
        }

        // upstream: flist.c:send_file_entry() line 654 - send_acl() called for
        // all non-symlink entries, including abbreviated hardlink followers.
        if self.preserve.acls && !entry.is_symlink() {
            let (mut access_acl, default_acl) = self
                .pending_acl
                .take()
                .unwrap_or_else(|| (RsyncAcl::from_mode(entry.mode()), None));

            // upstream: acls.c:657-658 - strip base entries derivable from mode
            access_acl.strip_perms_for_send(entry.mode());

            send_acl(
                writer,
                &access_acl,
                default_acl.as_ref(),
                entry.is_dir(),
                &mut self.acl_cache,
            )?;
        } else {
            // Discard unused pending ACL
            self.pending_acl = None;
        }

        // upstream: flist.c:send_file_entry() line 656 - send_xattr() called
        // for ALL entries including abbreviated hardlink followers.
        if self.preserve.xattrs {
            let list = entry.xattr_list().cloned().unwrap_or_default();
            // upstream: xattrs.c:send_xattr() - check cache for matching set
            let cached_index = self.find_matching_xattr(&list);
            send_xattr(writer, &list, cached_index, self.checksum_seed)?;
            if cached_index.is_none() {
                // upstream: xattrs.c:538 - rsync_xal_store() adds to cache
                let _ = self.xattr_cache.store(list);
            }
        }

        // upstream: flist.c:send_file_entry() line 676 - at the_end label,
        // metadata state (modtime, mode, uid, gid) is NOT updated for
        // abbreviated followers because the goto skips the metadata writes.
        if abbreviated {
            self.state.update_name(&name);
        } else {
            self.state.update(
                &name,
                entry.mode(),
                entry.mtime(),
                entry.uid().unwrap_or(0),
                entry.gid().unwrap_or(0),
            );
        }

        self.update_stats(entry);

        Ok(())
    }

    /// Finds a matching xattr set in the sender-side cache.
    ///
    /// Returns `Some(index)` if an identical xattr set has already been sent,
    /// allowing the writer to emit a cache reference instead of literal data.
    ///
    /// Comparison is by entry count plus name+value equality for each entry.
    /// This is a linear scan - upstream uses hash-based lookup, but the cache
    /// is typically small enough that linear scan is adequate.
    ///
    /// # Upstream Reference
    ///
    /// See `xattrs.c:find_matching_xattr()` - hash-based lookup in `rsync_xal_l`.
    fn find_matching_xattr(&self, list: &XattrList) -> Option<u32> {
        for i in 0..self.xattr_cache.len() {
            if let Some(cached) = self.xattr_cache.get(i) {
                if cached.len() != list.len() {
                    continue;
                }
                let all_match = cached.iter().zip(list.iter()).all(|(a, b)| {
                    a.name() == b.name() && a.datum_len() == b.datum_len() && a.datum() == b.datum()
                });
                if all_match {
                    return Some(i as u32);
                }
            }
        }
        None
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
