//! File list reading (decoding) from the rsync wire format.
//!
//! This module implements the receiver side of file list exchange, decoding
//! file entries as they arrive from the sender. The reader maintains compression
//! state to handle fields that are omitted when they match the previous entry.
//!
//! # Upstream Reference
//!
//! See `flist.c:recv_file_entry()` for the canonical wire format decoding.

mod extras;
mod flags;
mod metadata;
mod name;
#[cfg(test)]
mod tests;

use std::io::{self, Read};
use std::path::Path;

use logging::debug_log;

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::acl::{AclCache, receive_acl_cached};
use crate::codec::{ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::xattr::XattrCache;

use super::entry::FileEntry;
use super::flags::FileFlags;
use super::intern::PathInterner;
use super::state::{FileListCompressionState, FileListStats};

pub use flags::FlagsResult;
pub(crate) use metadata::MetadataResult;

/// State maintained while reading a file list from the wire.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This reader maintains the necessary state
/// to decode these compressed entries.
///
/// # Upstream Reference
///
/// Mirrors the static local state in `flist.c:recv_file_entry()` - the
/// `lastname`, `modtime`, `mode`, `uid`, `gid`, `rdev`, and `rdev_major`
/// variables that persist across calls during `recv_file_list()`.
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
    /// Wire NDX start of the current flist segment.
    ///
    /// Used to distinguish abbreviated vs unabbreviated hardlink followers.
    /// Abbreviated followers (leader in same segment, idx >= ndx_start) have
    /// metadata skipped on wire. Unabbreviated followers carry full metadata.
    /// upstream: flist.c:recv_file_entry() line 793
    ndx_start: i32,
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
            ndx_start: 0,
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
            ndx_start: 0,
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
    #[inline]
    #[must_use]
    pub const fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Sets whether extended attributes should be read from the wire.
    ///
    /// When enabled, xattr indices are read after ACL indices.
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

    /// Sets the wire NDX start of the current flist segment.
    ///
    /// Must be called before reading entries in each flist segment so that
    /// abbreviated vs unabbreviated hardlink followers are distinguished
    /// correctly on the wire.
    /// upstream: flist.c:recv_file_entry() uses `flist->ndx_start`
    pub fn set_ndx_start(&mut self, ndx_start: i32) {
        self.ndx_start = ndx_start;
    }

    /// Prepares the reader for a new incremental flist segment.
    ///
    /// Updates ndx_start for the new segment but preserves compression state.
    /// upstream: `recv_file_entry()` uses static variables for name compression,
    /// uid, gid, etc. These statics persist across `recv_file_list()` calls -
    /// sub-list entries can reference names from previous segments via the
    /// `XMIT_SAME_NAME` flag. Only the segment index base changes.
    pub fn reset_for_new_segment(&mut self, ndx_start: i32) {
        self.ndx_start = ndx_start;
    }

    /// Returns true if this is an abbreviated hardlink follower whose metadata
    /// was skipped on the wire (leader is in the same flist segment).
    ///
    /// Unabbreviated followers (leader in a previous flist segment) carry full
    /// metadata on the wire and must NOT skip reading it.
    /// upstream: flist.c:recv_file_entry() line 793
    #[inline]
    pub(crate) fn is_abbreviated_follower(
        &self,
        flags: FileFlags,
        hardlink_idx: Option<u32>,
    ) -> bool {
        if !flags.hlinked() || flags.hlink_first() {
            return false;
        }
        match hardlink_idx {
            Some(idx) => (idx as i32) >= self.ndx_start,
            None => false,
        }
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
        self.read_entry_with_flist(reader, &[])
    }

    /// Reads the next file entry, using `segment_entries` to resolve abbreviated
    /// hardlink followers.
    ///
    /// When an abbreviated follower is encountered (leader in the same flist
    /// segment), its metadata is copied from the leader entry and the
    /// compression state is updated to stay in sync with the sender.
    ///
    /// `segment_entries` must contain all entries already read in the current
    /// flist segment (i.e., entries at indices `0..` corresponding to wire
    /// NDXes `ndx_start..`).
    ///
    /// # Upstream Reference
    ///
    /// `flist.c:recv_file_entry()` lines 793-822 - receiver copies metadata
    /// from leader and updates static compression variables.
    pub fn read_entry_with_flist<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        segment_entries: &[FileEntry],
    ) -> io::Result<Option<FileEntry>> {
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

        let name = self.read_name(reader, flags)?;

        // upstream: flist.c:1873 - sender rejects empty names; we enforce the
        // same invariant as defense-in-depth against a malicious sender.
        if name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "received file entry with zero-length filename",
            ));
        }

        // Hardlink index must come immediately after name on wire.
        // upstream: flist.c:recv_file_entry() "goto create_object" for followers.
        let hardlink_idx = self.read_hardlink_idx(reader, flags)?;

        // Abbreviated followers (leader in same flist segment) have metadata
        // copied from the leader; unabbreviated followers carry full metadata.
        // upstream: flist.c:recv_file_entry() lines 793-822
        let (size, metadata, link_target, rdev, hardlink_dev_ino, checksum) =
            if self.is_abbreviated_follower(flags, hardlink_idx) {
                // upstream: flist.c:794 - look up leader in the current segment
                // and copy its metadata. The sender's static compression variables
                // (mode, modtime, uid, gid, atime) were already updated from the
                // leader's data before `goto the_end`, so the receiver must mirror
                // this by updating its compression state from the leader too.
                let leader_local_idx = (hardlink_idx.expect("abbreviated follower has hardlink_idx")
                    as i32
                    - self.ndx_start) as usize;

                if let Some(leader) = segment_entries.get(leader_local_idx) {
                    // upstream: flist.c:795-810 - copy fields from leader
                    let leader_mode = leader.mode();
                    let leader_mtime = leader.mtime();
                    let leader_uid = leader.uid();
                    let leader_gid = leader.gid();
                    let leader_atime = leader.atime();

                    // Update compression state to match sender's statics
                    // upstream: sender updates mode/modtime/uid/gid/atime at
                    // flist.c:430-493 BEFORE goto the_end
                    self.state.update_mode(leader_mode);
                    self.state.update_mtime(leader_mtime);
                    if let Some(uid) = leader_uid {
                        self.state.update_uid(uid);
                    }
                    if let Some(gid) = leader_gid {
                        self.state.update_gid(gid);
                    }
                    if (leader_mode & 0o170000) != 0o040000 {
                        self.state.update_atime(leader_atime);
                    }

                    (
                        leader.size(),
                        MetadataResult {
                            mtime: leader_mtime,
                            nsec: leader.mtime_nsec(),
                            mode: leader_mode,
                            uid: leader_uid,
                            gid: leader_gid,
                            user_name: None,
                            group_name: None,
                            atime: if leader_atime != 0 {
                                Some(leader_atime)
                            } else {
                                None
                            },
                            atime_nsec: leader.atime_nsec(),
                            crtime: None,
                            content_dir: (leader_mode & 0o170000) == 0o040000,
                        },
                        leader.link_target().cloned(),
                        leader.rdev_major().zip(leader.rdev_minor()),
                        None,
                        leader.checksum().map(|s| s.to_vec()),
                    )
                } else {
                    // Leader not available (segment_entries not provided or
                    // index out of range) - fall back to zero metadata.
                    // Compression state is NOT updated, which may cause
                    // subsequent decode errors if the caller didn't pass
                    // segment_entries.
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
                }
            } else {
                let size = self.read_size(reader)?;
                let metadata = self.read_metadata(reader, flags)?;
                let rdev = self.read_rdev(reader, metadata.mode, flags)?;
                let link_target = self.read_symlink_target(reader, metadata.mode)?;
                let hardlink_dev_ino = self.read_hardlink_dev_ino(reader, flags, metadata.mode)?;
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

        // upstream: flist.c recv_file_entry() iconv_buf(ic_recv, ...)
        // Decode the wire-charset filename bytes to local-charset bytes via
        // the configured FilenameConverter. With no converter (default), this
        // is a no-op; the prefix-compression buffer continues to hold the
        // unconverted wire bytes so subsequent entries share the prefix as the
        // sender intended (mirrors upstream's `lastname` semantics).
        let converted_name = self.apply_encoding_conversion(name)?;

        // upstream: flist.c:756-760 - clean_fname(CFN_REFUSE_DOT_DOT_DIRS)
        // then reject leading '/' when !relative_paths.
        // In --relative mode, leading slashes are stripped instead.
        let cleaned_name = self.clean_and_validate_name(converted_name)?;

        // Construct entry from raw bytes (avoids UTF-8 validation on Unix)
        let mut entry = FileEntry::from_raw_bytes(
            cleaned_name,
            size,
            metadata.mode,
            metadata.mtime,
            metadata.nsec,
            flags,
        );

        // Intern the dirname so entries in the same directory share
        // a single Arc<Path> allocation instead of each holding a separate copy.
        // This mirrors upstream rsync's shared dirname pointer pool.
        let parent = entry.path().parent().filter(|p| !p.as_os_str().is_empty());
        let interned_dirname = match parent {
            Some(p) => self.dirname_interner.intern(p),
            None => self.dirname_interner.intern(Path::new("")),
        };
        entry.set_dirname(interned_dirname);

        if let Some(target) = link_target {
            entry.set_link_target(target);
        }
        if let Some((major, minor)) = rdev {
            entry.set_rdev(major, minor);
        }
        if let Some(idx) = hardlink_idx {
            entry.set_hardlink_idx(idx);
        }
        if let Some(uid) = metadata.uid {
            entry.set_uid(uid);
        }
        if let Some(gid) = metadata.gid {
            entry.set_gid(gid);
        }
        if let Some(name) = metadata.user_name {
            entry.set_user_name(name);
        }
        if let Some(name) = metadata.group_name {
            entry.set_group_name(name);
        }
        if let Some(atime) = metadata.atime {
            entry.set_atime(atime);
            entry.set_atime_nsec(metadata.atime_nsec);
        }
        if let Some(crtime) = metadata.crtime {
            entry.set_crtime(crtime);
        }
        if entry.is_dir() {
            entry.set_content_dir(metadata.content_dir);
        }
        if let Some((dev, ino)) = hardlink_dev_ino {
            entry.set_hardlink_dev(dev);
            entry.set_hardlink_ino(ino);
        }
        if let Some(sum) = checksum {
            entry.set_checksum(sum);
        }

        // Read ACLs from the wire (after checksum, before xattrs).
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

        // Read xattr index/data from wire (after ACLs).
        // upstream: flist.c:1209-1212 - receive_xattr() is called after
        // receive_acl() and runs for ALL entries including hardlink followers.
        if self.preserve_xattrs {
            let xattr_ndx =
                self.xattr_cache
                    .receive_xattr(reader, self.am_root, self.xattr_level)?;
            entry.set_xattr_ndx(xattr_ndx);
        }

        // Update statistics
        self.update_stats(&entry);

        debug_log!(
            Flist,
            2,
            "recv_file_entry: {:?} size={} mode={:o}",
            entry.name(),
            entry.size(),
            entry.mode()
        );
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
}

/// Convenience function for reading individual entries without maintaining
/// reader state. For reading multiple entries, use [`FileListReader`] to
/// benefit from cross-entry compression.
///
/// Returns `Ok(Some(entry))` on success, `Ok(None)` at end-of-list, or
/// `Err` on I/O or protocol error.
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` for the canonical wire format decoding.
pub fn read_file_entry<R: Read>(
    reader: &mut R,
    protocol: ProtocolVersion,
) -> io::Result<Option<FileEntry>> {
    let mut list_reader = FileListReader::new(protocol);
    list_reader.read_entry(reader)
}
