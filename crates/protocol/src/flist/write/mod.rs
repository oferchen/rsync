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
    /// Whether the negotiated session converts symlink TARGETS through iconv.
    ///
    /// Filename iconv (`iconv`) and symlink-target iconv are gated separately by
    /// upstream. The sender only transcodes a symlink target when `--iconv` is
    /// active AND the peer negotiated `CF_SYMLINK_ICONV` (the `'s'` capability).
    /// Against a proto-30 / pre-3.1 peer that lacks the capability, targets are
    /// sent as raw local bytes even while filenames are transcoded.
    ///
    /// upstream: compat.c:765-767 `sender_symlink_iconv = iconv_opt && (...)`,
    /// applied at flist.c:1642.
    symlink_iconv: bool,
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
    /// Whether to write user/group name strings for named ACL entries.
    ///
    /// Upstream writes ACL names only when `inc_recurse && !numeric_ids`
    /// (`acls.c:597`); otherwise the receiver remaps ids through the id-list.
    acl_send_names: bool,
    /// Whether to emit inline `XMIT_USER_NAME_FOLLOWS`/`XMIT_GROUP_NAME_FOLLOWS`
    /// owner names in each file entry.
    ///
    /// Upstream sets these flags only under incremental recursion
    /// (`flist.c:481-482,491-492`: `if (inc_recurse && user_name)`). Without
    /// `inc_recurse` the names ride exclusively in the trailing id-list
    /// (`send_id_lists`/`recv_id_list`, uidlist.c), so emitting them inline as
    /// well diverges from upstream's wire encoding. Defaults to `false`; the
    /// generator sets it to the negotiated `inc_recurse` value.
    name_follows: bool,
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
            symlink_iconv: false,
            use_varint_flags: false,
            use_safe_file_list: protocol.safe_file_list_always_enabled(),
            first_ndx: 0,
            acl_cache: AclCache::new(),
            pending_acl: None,
            acl_send_names: false,
            name_follows: false,
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
            symlink_iconv: false,
            use_varint_flags: compat_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            use_safe_file_list: compat_flags.contains(CompatibilityFlags::SAFE_FILE_LIST)
                || protocol.safe_file_list_always_enabled(),
            first_ndx: 0,
            acl_cache: AclCache::new(),
            pending_acl: None,
            acl_send_names: false,
            name_follows: false,
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

    /// Sets whether user/group name strings are written for named ACL entries.
    ///
    /// Upstream sends ACL names only when `inc_recurse && !numeric_ids`
    /// (`acls.c:597`); otherwise the receiver remaps ids through the id-list.
    #[inline]
    #[must_use]
    pub const fn with_acl_send_names(mut self, send_names: bool) -> Self {
        self.acl_send_names = send_names;
        self
    }

    /// Sets whether inline owner names (`XMIT_USER_NAME_FOLLOWS`/
    /// `XMIT_GROUP_NAME_FOLLOWS`) are emitted per file entry.
    ///
    /// Upstream sets these flags only under incremental recursion
    /// (`flist.c:481-482,491-492`: `if (inc_recurse && user_name)`). Callers
    /// pass the negotiated `inc_recurse` value. When `false`, owner names are
    /// carried solely by the trailing id-list (`send_id_lists`), matching
    /// upstream's non-incremental encoding.
    #[inline]
    #[must_use]
    pub const fn with_name_follows(mut self, name_follows: bool) -> Self {
        self.name_follows = name_follows;
        self
    }

    /// Returns the sender-side ACL cache accumulated across written entries.
    ///
    /// Used to feed named-entry user/group ids into the shared uid/gid id-list
    /// before it is transmitted, mirroring upstream `add_uid`/`add_gid` in
    /// `send_ida_entries` (`acls.c:592-595`).
    #[inline]
    #[must_use]
    pub const fn acl_cache(&self) -> &AclCache {
        &self.acl_cache
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

    /// Sets whether symlink TARGETS are transcoded through iconv.
    ///
    /// Must reflect the negotiated `CF_SYMLINK_ICONV` capability AND an active
    /// `--iconv`. When `false`, symlink targets are written as raw local bytes
    /// even if a filename converter is attached.
    ///
    /// upstream: compat.c:765-767 `sender_symlink_iconv`, applied at flist.c:1642.
    #[inline]
    #[must_use]
    pub const fn with_symlink_iconv(mut self, symlink_iconv: bool) -> Self {
        self.symlink_iconv = symlink_iconv;
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
    ///
    /// Abbreviation is a protocol 30+ feature: upstream only skips metadata
    /// after writing `first_hlink_ndx` (`goto the_end`), and `first_hlink_ndx`
    /// is set only when `protocol_version >= 30` (flist.c:517-587). For
    /// protocols 28-29 hardlinks are identified by trailing `(dev, ino)` pairs
    /// and every entry carries full metadata; abbreviating a follower there
    /// desyncs the receiver (it still reads full metadata + dev/ino).
    ///
    /// upstream: flist.c:send_file_entry() lines 585-587 (`goto the_end`)
    #[inline]
    fn is_abbreviated_follower(&self, entry: &FileEntry, xflags: u32) -> bool {
        if self.protocol.as_u8() < 30 {
            return false;
        }
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
                self.acl_send_names,
            )?;
        } else {
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
