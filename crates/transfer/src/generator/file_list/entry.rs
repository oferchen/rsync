//! File entry construction and metadata collection for the generator role.
//!
//! Builds `FileEntry` values from filesystem metadata, handling platform-specific
//! fields (mode, uid/gid, devices, symlinks, hardlink dev/ino).
//!
//! # Upstream Reference
//!
//! - `flist.c:make_file()` - determines file type and populates `file_struct`
//! - `flist.c:readlink_stat()` - symlink target resolution
//! - `xattrs.c:get_stat_xattr()` - fake-super override applied via
//!   `x_lstat()` before `make_file()` consumes the stat values

use std::io;
use std::path::{Path, PathBuf};

use protocol::flist::FileEntry;

use super::super::GeneratorContext;

impl GeneratorContext {
    /// Creates a `FileEntry` from filesystem metadata for wire transmission.
    ///
    /// Populates mode, mtime, uid/gid, atime/crtime, symlink targets, device numbers,
    /// and hardlink dev/ino fields based on the active preservation flags.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:make_file()` - determines file type and populates the `file_struct`
    /// - Device files (block/char) use `new_block_device`/`new_char_device` with rdev fields
    /// - Special files (FIFOs/sockets) use `new_fifo`/`new_socket`
    pub(in crate::generator) fn create_entry(
        &self,
        full_path: &Path,
        relative_path: PathBuf,
        metadata: &std::fs::Metadata,
    ) -> io::Result<FileEntry> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let file_type = metadata.file_type();

        // Native Windows reparse-point detection. `std::fs::FileType::is_symlink`
        // only returns `true` for `IO_REPARSE_TAG_SYMLINK` and
        // `IO_REPARSE_TAG_MOUNT_POINT`; tags like `IO_REPARSE_TAG_AF_UNIX`,
        // `IO_REPARSE_TAG_ONEDRIVE`, and the cloud-files range therefore slip
        // through and would otherwise serialise as plain regular files. The
        // `FILE_ATTRIBUTE_REPARSE_POINT` attribute catches every reparse
        // shape; we route the entry through the symlink branch so the WPC-8
        // classifier (`metadata::windows::reparse::classify_path`) governs the
        // wire emission rather than the surface `FileType` probe.
        //
        // Upstream rsync runs through Cygwin on Windows and treats every
        // reparse point as a POSIX symbolic link; this mirror keeps the wire
        // output compatible without losing the classifier's distinction.
        #[cfg(windows)]
        let is_reparse_point = {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        };

        // upstream: xattrs.c:get_stat_xattr() - when `--fake-super` is in
        // effect, a previous fake-super receiver may have recorded the real
        // ownership, permissions, and device numbers in the source file's
        // `user.rsync.%stat` xattr (the on-disk file is a placeholder).
        // Decoding it here lets the sender round-trip the original metadata
        // instead of forwarding the placeholder uid/gid/mode/dev. The
        // override carries POSIX-only fields (uid/gid/rdev), so the Windows
        // path never consults it - we skip the lookup entirely there.
        #[cfg(unix)]
        let fake_super_override = self.fake_super_override(full_path, metadata);

        // Windows reparse-point branch. Run before the regular-file probe so
        // non-symlink reparse points (cloud placeholders, AF_UNIX sockets,
        // opaque vendor tags) do not slip into the regular-file emission.
        // Junctions and mount-points already pass through
        // `file_type.is_symlink()`, but routing them here too lets the
        // classifier choose the wire shape for every flavour in one place.
        #[cfg(windows)]
        let reparse_kind = if is_reparse_point {
            metadata::windows::classify_path(full_path).ok()
        } else {
            None
        };

        let mut entry = if file_type.is_file() && {
            #[cfg(windows)]
            {
                !is_reparse_point
            }
            #[cfg(not(windows))]
            {
                true
            }
        } {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };

            // upstream: xattrs.c:get_stat_xattr() - the xattr's mode replaces
            // the entire st_mode (type + perms) so a regular placeholder file
            // can masquerade as a device/symlink/special on the wire.
            #[cfg(unix)]
            let entry = if let Some(stat) = fake_super_override.as_ref() {
                build_entry_from_fake_super(relative_path, metadata.len(), stat)
            } else {
                FileEntry::new_file(relative_path, metadata.len(), mode)
            };
            #[cfg(not(unix))]
            let entry = FileEntry::new_file(relative_path, metadata.len(), mode);
            entry
        } else if file_type.is_dir() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = 0o755;

            // upstream: flist.c:1465 - `file->len32 = (uint32)st.st_size` runs for
            // every entry; only devices/specials are zeroed (flist.c:1452-1454).
            // Directories therefore carry their on-disk inode size on the wire,
            // which feeds `--list-only`, `%l`, and the `--stats` total.
            let mut entry = FileEntry::new_directory(relative_path, mode);
            entry.set_size(metadata.len());
            entry
        } else if file_type.is_symlink() || {
            #[cfg(windows)]
            {
                is_reparse_point
            }
            #[cfg(not(windows))]
            {
                false
            }
        } {
            // upstream: flist.c:readlink_stat() returns the link target as
            // recorded on disk. On Windows the kernel exposes junctions and
            // symlinks through `read_link`; cloud placeholders and AF_UNIX
            // sockets do not have a portable target and we fall back to an
            // empty path so the receiver materialises a stub link rather than
            // dropping the entry. The WPC-8 classification is consulted only
            // to decide whether we trust `read_link` (Symlink/Junction) or
            // emit an empty target (OneDrive/AfUnix/Other); the on-wire
            // type is always SYMLINK for Cygwin parity.
            #[cfg(windows)]
            let raw_target = match reparse_kind {
                Some(metadata::windows::ReparseKind::Symlink)
                | Some(metadata::windows::ReparseKind::Junction)
                | Some(metadata::windows::ReparseKind::MountPoint)
                | None => std::fs::read_link(full_path).unwrap_or_else(|_| PathBuf::from("")),
                Some(metadata::windows::ReparseKind::OneDrive)
                | Some(metadata::windows::ReparseKind::AfUnix)
                | Some(metadata::windows::ReparseKind::Other(_)) => PathBuf::from(""),
            };
            #[cfg(not(windows))]
            let raw_target = std::fs::read_link(full_path).unwrap_or_else(|_| PathBuf::from(""));

            // upstream: flist.c:222-226 - sender strips the `/rsyncd-munged/`
            // prefix from the readlink result when the daemon module has
            // `munge symlinks = yes`, restoring the original target before it
            // is sent on the wire. The matching prepend on the receive side
            // re-applies the prefix when the link is materialized on disk.
            let target = strip_symlink_munge_prefix(self.config.munge_symlinks, raw_target);

            // upstream: flist.c:1465 - symlinks carry `st_size`, which lstat
            // reports as the byte length of the link target. The receiver gets
            // the target separately, so this length is purely the size shown by
            // `--list-only`/`%l` and summed into the `--stats` total.
            let mut entry = FileEntry::new_symlink(relative_path, target);
            entry.set_size(metadata.len());
            entry
        } else {
            // Device and special file types (Unix only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                let mode = metadata.mode() & 0o7777;
                if file_type.is_block_device() {
                    let (major, minor) = rdev_to_major_minor(metadata.rdev());
                    FileEntry::new_block_device(relative_path, mode, major, minor)
                } else if file_type.is_char_device() {
                    let (major, minor) = rdev_to_major_minor(metadata.rdev());
                    FileEntry::new_char_device(relative_path, mode, major, minor)
                } else if file_type.is_fifo() {
                    FileEntry::new_fifo(relative_path, mode)
                } else if file_type.is_socket() {
                    FileEntry::new_socket(relative_path, mode)
                } else {
                    FileEntry::new_file(relative_path, 0, 0o644)
                }
            }
            #[cfg(not(unix))]
            {
                FileEntry::new_file(relative_path, 0, 0o644)
            }
        };

        // upstream: flist.c:make_file() - set mtime
        #[cfg(unix)]
        {
            entry.set_mtime(metadata.mtime(), metadata.mtime_nsec() as u32);
        }
        #[cfg(not(unix))]
        {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_mtime(duration.as_secs() as i64, duration.subsec_nanos());
                }
            }
        }

        // Set access time if preserving (upstream: flist.c:489-494)
        #[cfg(unix)]
        if self.config.flags.atimes && !entry.is_dir() {
            entry.set_atime(metadata.atime());
        }
        #[cfg(not(unix))]
        if self.config.flags.atimes && !entry.is_dir() {
            if let Ok(atime) = metadata.accessed() {
                if let Ok(duration) = atime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_atime(duration.as_secs() as i64);
                }
            }
        }

        // Set creation time if preserving (upstream: flist.c:495-498)
        if self.config.flags.crtimes {
            if let Ok(crtime) = metadata.created() {
                if let Ok(duration) = crtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_crtime(duration.as_secs() as i64);
                }
            }
        }

        // upstream: flist.c:make_file() - set uid/gid
        // When the fake-super xattr overrode the stat values, prefer the
        // decoded uid/gid so a round-trip through a fake-super sender
        // preserves the original ownership.
        #[cfg(unix)]
        if self.config.flags.owner {
            let uid = fake_super_override
                .as_ref()
                .map_or_else(|| metadata.uid(), |s| s.uid);
            entry.set_uid(uid);
            // upstream: flist.c:466-470 - add_uid() looks up name for inline
            // sending via XMIT_USER_NAME_FOLLOWS when INC_RECURSE is active.
            // Without names, the receiver can't map uid->name on the remote.
            if !self.config.flags.numeric_ids {
                if let Ok(Some(name_bytes)) = metadata::id_lookup::lookup_user_name(uid) {
                    if let Ok(name) = String::from_utf8(name_bytes) {
                        entry.set_user_name(name);
                    }
                }
            }
        }
        #[cfg(unix)]
        if self.config.flags.group {
            let gid = fake_super_override
                .as_ref()
                .map_or_else(|| metadata.gid(), |s| s.gid);
            entry.set_gid(gid);
            // upstream: flist.c:476-480 - add_gid() looks up name for inline
            // sending via XMIT_GROUP_NAME_FOLLOWS when INC_RECURSE is active.
            if !self.config.flags.numeric_ids {
                if let Ok(Some(name_bytes)) = metadata::id_lookup::lookup_group_name(gid) {
                    if let Ok(name) = String::from_utf8(name_bytes) {
                        entry.set_group_name(name);
                    }
                }
            }
        }

        // Store dev/ino for hardlink detection (post-sort assignment).
        // upstream: flist.c:make_file() stores tmp_dev/tmp_ino when preserve_hard_links
        #[cfg(unix)]
        if self.config.flags.hard_links && metadata.nlink() > 1 && !metadata.is_dir() {
            entry.set_hardlink_dev(metadata.dev() as i64);
            entry.set_hardlink_ino(metadata.ino() as i64);
        }

        // upstream: flist.c:make_file() -> get_xattr() reads xattrs for -X mode
        #[cfg(unix)]
        if self.config.flags.xattrs {
            // upstream: xattrs.c:303-334 - get_xattr() only reads for regular files,
            // dirs, symlinks (if preserve_links), specials (if preserve_specials),
            // and devices (if preserve_devices).
            let should_read = file_type.is_file()
                || file_type.is_dir()
                || (file_type.is_symlink() && self.config.flags.links);

            if should_read {
                // Follow symlinks only for non-symlink entries (lgetxattr for symlinks)
                let follow = !file_type.is_symlink();
                match metadata::read_xattrs_for_wire(
                    full_path,
                    follow,
                    false, // am_root: sender on Linux non-root reads user.* only
                    self.checksum_seed,
                ) {
                    Ok(list) => {
                        if !list.is_empty() {
                            entry.set_xattr_list(list);
                        }
                    }
                    Err(_) => {
                        // Non-fatal: silently skip, matching upstream behavior
                        // where xattr read failures don't abort the transfer
                    }
                }
            }
        }

        // Windows ACL collection: when --acls is on (and on Windows the
        // `acl` feature is compiled in) read the full SDDL security
        // descriptor and attach it to the entry under the reserved
        // `user.win32.security_descriptor` xattr slot. The receiver routes
        // the slot through `apply_sddl_from_xattrs` so Windows->Windows
        // transfers preserve the descriptor verbatim; non-Windows
        // receivers drop the slot.
        #[cfg(all(feature = "acl", windows))]
        if self.config.flags.acls {
            let should_read = file_type.is_file() || file_type.is_dir();
            if should_read {
                if let Ok(Some(sddl_entry)) = metadata::sddl_xattr_entry(full_path) {
                    let mut list = entry.xattr_list().cloned().unwrap_or_default();
                    list.push(sddl_entry);
                    list.sort_by_name();
                    entry.set_xattr_list(list);
                }
            }
        }

        // upstream: clientserver.c:rsync_module() arms `daemon_chmod_modes`
        // and flist.c:make_file() applies it as the file_struct is built so
        // the wire-emitted mode reflects the daemon's `outgoing chmod = SPEC`
        // directive. We mirror that ordering: rewrite the entry's mode after
        // every other flist field has been populated but before the caller
        // serialises it. The chmod parser preserves the file-type bits, so
        // the entry's S_IFREG/S_IFDIR/etc. classification is untouched.
        if let Some(modifiers) = self.config.daemon_outgoing_chmod.as_ref() {
            let rewritten = modifiers.apply(entry.mode(), file_type);
            entry.set_mode(rewritten);
        }

        Ok(entry)
    }

    /// Reads the source-side `user.rsync.%stat` xattr when fake-super is active.
    ///
    /// Returns the decoded [`metadata::FakeSuperStat`] only when:
    /// - `--fake-super` (or daemon `fake super = yes`) is in effect,
    /// - the on-disk entry is neither a device nor a special file (matching
    ///   upstream's `IS_DEVICE(fst->st_mode) || IS_SPECIAL(fst->st_mode)`
    ///   early-return in `xattrs.c:get_stat_xattr()`), and
    /// - the xattr exists and decodes successfully.
    ///
    /// Mirrors the override path upstream applies via `x_lstat()`/`x_stat()`
    /// before `make_file()` reads the stat values, so a round-trip through a
    /// fake-super sender preserves the original ownership/perms/device.
    ///
    /// # Upstream Reference
    ///
    /// - `xattrs.c:1127 get_stat_xattr()`
    /// - `xattrs.c:1258 x_lstat()` (called from `flist.c:link_stat()`)
    #[cfg(unix)]
    fn fake_super_override(
        &self,
        full_path: &Path,
        metadata: &std::fs::Metadata,
    ) -> Option<metadata::FakeSuperStat> {
        if !self.config.fake_super {
            return None;
        }
        // upstream: xattrs.c:1133 - skip when the on-disk file is already a
        // device or special; the xattr only applies to regular placeholders.
        use std::os::unix::fs::FileTypeExt;
        let ft = metadata.file_type();
        if ft.is_block_device() || ft.is_char_device() || ft.is_fifo() || ft.is_socket() {
            return None;
        }
        // Silently swallow read/decode errors: upstream's `get_stat_xattr`
        // logs but does not abort on ENOTSUP/ENOATTR, and any other error
        // here is treated like a missing xattr so the stat-derived values
        // remain in use.
        metadata::load_fake_super(full_path).ok().flatten()
    }

    // The non-Unix branch deliberately omits `fake_super_override`. The only
    // caller is `#[cfg(unix)]`-gated in `build_file_entry`, so a Windows
    // stub would be dead code (rejected by `-D dead-code`). Adding a Windows
    // call site later should reintroduce a stub here.
}

/// Builds the wire `FileEntry` for a regular placeholder file whose
/// `user.rsync.%stat` xattr decoded successfully.
///
/// The xattr's mode encodes the *effective* file type (regular, device,
/// symlink, fifo, socket) plus permission bits. For devices, the decoded
/// `rdev` major/minor populate the wire fields. When the xattr's mode does
/// not encode a recognised type, fall back to a regular file with the
/// decoded permission bits.
///
/// # Upstream Reference
///
/// - `xattrs.c:1172 from_wire_mode()` - the xattr's mode replaces st_mode
/// - `flist.c:make_file()` - downstream branches pick the wire encoding
///   from the (now overridden) mode
#[cfg(unix)]
fn build_entry_from_fake_super(
    relative_path: PathBuf,
    size: u64,
    stat: &metadata::FakeSuperStat,
) -> FileEntry {
    use protocol::flist::FileType;

    let perm_bits = stat.mode & 0o7777;
    let (rdev_major, rdev_minor) = stat.rdev.unwrap_or((0, 0));

    match FileType::from_mode(stat.mode) {
        Some(FileType::Regular) | None => FileEntry::new_file(relative_path, size, perm_bits),
        Some(FileType::Directory) => FileEntry::new_directory(relative_path, perm_bits),
        Some(FileType::Symlink) => {
            // upstream: fake-super symlinks stash the target separately;
            // when the xattr alone is the source of truth, we emit an empty
            // target to match the placeholder content.
            FileEntry::new_symlink(relative_path, PathBuf::new())
        }
        Some(FileType::BlockDevice) => {
            FileEntry::new_block_device(relative_path, perm_bits, rdev_major, rdev_minor)
        }
        Some(FileType::CharDevice) => {
            FileEntry::new_char_device(relative_path, perm_bits, rdev_major, rdev_minor)
        }
        Some(FileType::Fifo) => FileEntry::new_fifo(relative_path, perm_bits),
        Some(FileType::Socket) => FileEntry::new_socket(relative_path, perm_bits),
    }
}

/// Extracts major and minor device numbers from a raw `rdev` value.
///
/// The layout differs by platform:
/// - **Linux**: Split encoding where major/minor span non-contiguous bits.
/// - **macOS/BSD**: Major in high byte, minor in low 24 bits.
///
/// # Upstream Reference
///
/// Mirrors glibc `major()`/`minor()` macros used by upstream rsync to populate
/// `rdev_major`/`rdev_minor` in `file_struct`.
#[cfg(all(unix, target_os = "linux"))]
pub(in crate::generator) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32);
    let minor = (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32);
    (major, minor)
}

/// Extracts major and minor device numbers from a raw `rdev` value (BSD/macOS).
///
/// BSD layout: major in bits 31-24, minor in bits 23-0.
#[cfg(all(unix, not(target_os = "linux")))]
pub(in crate::generator) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = (rdev >> 24) as u32;
    let minor = (rdev & 0xffffff) as u32;
    (major, minor)
}

/// Strips the `/rsyncd-munged/` prefix from a symlink target when munging is active.
///
/// Mirrors upstream `flist.c:222-226`: when the daemon module has
/// `munge symlinks = yes`, the sender restores the original target before it
/// is encoded onto the wire so the receiver can reapply the prefix on its
/// side. The receiver-side write path uses the matching
/// [`crate::receiver::directory::apply_symlink_munge_prefix`] helper.
///
/// Targets that lack the prefix pass through unchanged; this is the
/// `llen > SYMLINK_PREFIX_LEN && strncmp(...) == 0` branch in upstream's
/// `readlink_stat()`.
fn strip_symlink_munge_prefix(munge_symlinks: bool, target: PathBuf) -> PathBuf {
    if !munge_symlinks {
        return target;
    }
    let prefix_len = ::metadata::SYMLINK_MUNGE_PREFIX.len();
    // upstream: flist.c:222 - the strncmp guard requires `llen > SYMLINK_PREFIX_LEN`,
    // so a target consisting of exactly the prefix is left untouched.
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let bytes = target.as_os_str().as_bytes();
        let prefix = ::metadata::SYMLINK_MUNGE_PREFIX.as_bytes();
        if bytes.len() > prefix_len && bytes.starts_with(prefix) {
            let tail = OsStr::from_bytes(&bytes[prefix_len..]);
            return PathBuf::from(tail);
        }
        target
    }
    #[cfg(not(unix))]
    {
        match target.to_str() {
            Some(text)
                if text.len() > prefix_len
                    && text.starts_with(::metadata::SYMLINK_MUNGE_PREFIX) =>
            {
                PathBuf::from(&text[prefix_len..])
            }
            _ => target,
        }
    }
}

#[cfg(test)]
mod munge_strip_tests {
    use super::*;

    #[test]
    fn passes_through_when_disabled() {
        let original = PathBuf::from("/rsyncd-munged/etc/passwd");
        let returned = strip_symlink_munge_prefix(false, original.clone());
        assert_eq!(returned, original);
    }

    #[test]
    fn strips_prefix_when_enabled() {
        let original = PathBuf::from("/rsyncd-munged/etc/passwd");
        let returned = strip_symlink_munge_prefix(true, original);
        assert_eq!(returned, PathBuf::from("etc/passwd"));
    }

    #[test]
    fn passes_through_unprefixed_target_when_enabled() {
        let original = PathBuf::from("../sibling");
        let returned = strip_symlink_munge_prefix(true, original.clone());
        assert_eq!(returned, original);
    }

    #[test]
    fn does_not_strip_exact_prefix_match() {
        // upstream: flist.c:222 - `llen > SYMLINK_PREFIX_LEN` is strict, so a
        // target whose entire content is the prefix is left untouched. Without
        // the strict check we would emit an empty target onto the wire and
        // diverge from upstream byte-for-byte.
        let original = PathBuf::from("/rsyncd-munged/");
        let returned = strip_symlink_munge_prefix(true, original.clone());
        assert_eq!(returned, original);
    }
}

#[cfg(all(test, unix))]
mod fake_super_tests {
    //! Sender-side `user.rsync.%stat` consumption tests.
    //!
    //! Verifies that under `--fake-super` the source-stored xattr overrides
    //! the on-disk stat values when populating the wire file-list entry,
    //! matching upstream rsync 3.4.1 `xattrs.c:get_stat_xattr()` semantics.

    use super::*;
    use ::metadata::FakeSuperStat;
    use protocol::flist::FileType;

    #[test]
    fn build_from_fake_super_emits_regular_file_for_regular_mode() {
        let stat = FakeSuperStat {
            mode: 0o100644,
            uid: 1234,
            gid: 5678,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("a"), 42, &stat);
        assert_eq!(entry.file_type(), FileType::Regular);
        assert_eq!(entry.permissions() & 0o7777, 0o644);
        assert_eq!(entry.size(), 42);
    }

    #[test]
    fn build_from_fake_super_emits_block_device_from_mode_bits() {
        // 0o60660 = S_IFBLK | 0660
        let stat = FakeSuperStat {
            mode: 0o60660,
            uid: 0,
            gid: 6,
            rdev: Some((8, 0)),
        };
        let entry = build_entry_from_fake_super(PathBuf::from("sda"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::BlockDevice);
        assert_eq!(entry.permissions() & 0o7777, 0o660);
        assert_eq!(entry.rdev_major(), Some(8));
        assert_eq!(entry.rdev_minor(), Some(0));
    }

    #[test]
    fn build_from_fake_super_emits_char_device_from_mode_bits() {
        // 0o20666 = S_IFCHR | 0666
        let stat = FakeSuperStat {
            mode: 0o20666,
            uid: 0,
            gid: 0,
            rdev: Some((1, 3)),
        };
        let entry = build_entry_from_fake_super(PathBuf::from("null"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::CharDevice);
        assert_eq!(entry.rdev_major(), Some(1));
        assert_eq!(entry.rdev_minor(), Some(3));
    }

    #[test]
    fn build_from_fake_super_emits_fifo_from_mode_bits() {
        let stat = FakeSuperStat {
            mode: 0o10644,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("pipe"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Fifo);
    }

    #[test]
    fn build_from_fake_super_emits_socket_from_mode_bits() {
        let stat = FakeSuperStat {
            mode: 0o140755,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("sock"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Socket);
    }

    #[test]
    fn build_from_fake_super_emits_directory_from_mode_bits() {
        let stat = FakeSuperStat {
            mode: 0o40755,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("d"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Directory);
    }

    #[test]
    fn build_from_fake_super_emits_symlink_with_empty_target() {
        let stat = FakeSuperStat {
            mode: 0o120777,
            uid: 1000,
            gid: 1000,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("link"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Symlink);
    }

    #[test]
    fn build_from_fake_super_unknown_mode_falls_back_to_regular() {
        // No file-type bits set: treat as regular with the given perms.
        let stat = FakeSuperStat {
            mode: 0o0644,
            uid: 1,
            gid: 2,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("f"), 7, &stat);
        assert_eq!(entry.file_type(), FileType::Regular);
        assert_eq!(entry.size(), 7);
    }
}

#[cfg(all(test, unix, feature = "xattr"))]
mod fake_super_round_trip_tests {
    //! End-to-end sender override: place a fake-super xattr on a regular
    //! placeholder file, then verify `create_entry` consumes it and emits
    //! the decoded mode/uid/gid/rdev instead of the on-disk stat values.

    use super::super::super::GeneratorContext;
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use ::metadata::{FAKE_SUPER_XATTR, FakeSuperStat};
    use protocol::ProtocolVersion;
    use protocol::flist::FileType;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_generator(fake_super: bool, owner: bool, group: bool) -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let mut config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            fake_super,
            ..Default::default()
        };
        config.flags.owner = owner;
        config.flags.group = group;
        config.flags.numeric_ids = true; // skip uid/gid name lookups in tests
        GeneratorContext::new_for_test(&handshake, config)
    }

    fn write_placeholder_with_xattr(tmp: &TempDir, stat: &FakeSuperStat) -> PathBuf {
        let path = tmp.path().join("placeholder");
        std::fs::write(&path, b"x").unwrap();
        match xattr::set(&path, FAKE_SUPER_XATTR, stat.encode().as_bytes()) {
            Ok(()) => path,
            Err(e) => {
                // tmpfs / sandboxed filesystems may reject user.* xattrs; skip
                // the test gracefully so CI on such hosts does not flake.
                eprintln!("skipping: xattr unsupported on test filesystem: {e}");
                std::process::exit(0);
            }
        }
    }

    #[test]
    fn fake_super_off_returns_no_override() {
        let tmp = TempDir::new().unwrap();
        let stat = FakeSuperStat {
            mode: 0o100600,
            uid: 4321,
            gid: 8765,
            rdev: None,
        };
        let path = write_placeholder_with_xattr(&tmp, &stat);
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let ctx = make_generator(false, true, true);
        let entry = ctx
            .create_entry(&path, PathBuf::from("placeholder"), &meta)
            .unwrap();
        // Without --fake-super, the on-disk uid/gid (the test user) is sent.
        use std::os::unix::fs::MetadataExt;
        assert_eq!(entry.uid(), Some(meta.uid()));
        assert_eq!(entry.gid(), Some(meta.gid()));
        assert_eq!(entry.file_type(), FileType::Regular);
    }

    #[test]
    fn fake_super_on_override_uid_gid_for_regular_file() {
        let tmp = TempDir::new().unwrap();
        let stat = FakeSuperStat {
            mode: 0o100600,
            uid: 4321,
            gid: 8765,
            rdev: None,
        };
        let path = write_placeholder_with_xattr(&tmp, &stat);
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let ctx = make_generator(true, true, true);
        let entry = ctx
            .create_entry(&path, PathBuf::from("placeholder"), &meta)
            .unwrap();
        assert_eq!(entry.uid(), Some(4321), "uid must come from %stat xattr");
        assert_eq!(entry.gid(), Some(8765), "gid must come from %stat xattr");
        assert_eq!(entry.file_type(), FileType::Regular);
    }

    #[test]
    fn fake_super_on_promotes_regular_placeholder_to_block_device() {
        let tmp = TempDir::new().unwrap();
        let stat = FakeSuperStat {
            mode: 0o60660,
            uid: 0,
            gid: 6,
            rdev: Some((8, 0)),
        };
        let path = write_placeholder_with_xattr(&tmp, &stat);
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let ctx = make_generator(true, true, true);
        let entry = ctx
            .create_entry(&path, PathBuf::from("sda"), &meta)
            .unwrap();
        assert_eq!(entry.file_type(), FileType::BlockDevice);
        assert_eq!(entry.uid(), Some(0));
        assert_eq!(entry.gid(), Some(6));
        assert_eq!(entry.rdev_major(), Some(8));
        assert_eq!(entry.rdev_minor(), Some(0));
    }

    #[test]
    fn fake_super_on_without_xattr_falls_back_to_stat() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("plain");
        std::fs::write(&path, b"x").unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let ctx = make_generator(true, true, true);
        let entry = ctx
            .create_entry(&path, PathBuf::from("plain"), &meta)
            .unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(entry.uid(), Some(meta.uid()));
        assert_eq!(entry.gid(), Some(meta.gid()));
        assert_eq!(entry.file_type(), FileType::Regular);
    }

    #[test]
    fn fake_super_decoded_format_matches_upstream_byte_for_byte() {
        // upstream: xattrs.c:1233 - snprintf("%o %u,%u %u:%u", ...)
        let stat = FakeSuperStat {
            mode: 0o100644,
            uid: 1234,
            gid: 5678,
            rdev: None,
        };
        assert_eq!(stat.encode(), "100644 0,0 1234:5678");
    }
}

#[cfg(all(test, unix))]
mod daemon_outgoing_chmod_tests {
    //! Daemon `outgoing chmod = SPEC` regression: the sender must rewrite the
    //! wire-emitted mode for each file list entry when the daemon module has
    //! an `outgoing chmod` directive configured. Mirrors upstream
    //! `clientserver.c:rsync_module()` arming `daemon_chmod_modes` and
    //! `flist.c:make_file()` applying them as file_struct values are built.

    use super::super::super::GeneratorContext;
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use ::metadata::ChmodModifiers;
    use protocol::ProtocolVersion;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_generator(outgoing_chmod: Option<ChmodModifiers>) -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let mut config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            daemon_outgoing_chmod: outgoing_chmod,
            ..Default::default()
        };
        config.flags.numeric_ids = true;
        GeneratorContext::new_for_test(&handshake, config)
    }

    /// `outgoing chmod = Fg-r` must clear the group-read bit on the wire-emitted
    /// mode for every file entry the sender constructs. The on-disk source
    /// retains its original permissions; only the file list entry is rewritten.
    #[test]
    fn outgoing_chmod_clears_group_read_bit_on_wire() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("source.txt");
        std::fs::write(&path, b"payload").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664))
            .expect("set source perms");

        let modifiers = ChmodModifiers::parse("Fg-r").expect("parse chmod spec");
        let ctx = make_generator(Some(modifiers));
        let meta = std::fs::symlink_metadata(&path).expect("metadata");
        let entry = ctx
            .create_entry(&path, PathBuf::from("source.txt"), &meta)
            .expect("create_entry");

        // Group-read (0o040) must be cleared; other bits left intact.
        let perms = entry.permissions() & 0o7777;
        assert_eq!(perms & 0o040, 0, "group-read must be cleared on wire");
        assert_eq!(perms, 0o624, "Fg-r rewrites 0o664 to 0o624");
    }

    /// When no `outgoing chmod` is configured, `create_entry` must emit the
    /// on-disk mode verbatim - no rewrite, no silent default.
    #[test]
    fn no_outgoing_chmod_leaves_mode_untouched() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("source.txt");
        std::fs::write(&path, b"payload").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664))
            .expect("set source perms");

        let ctx = make_generator(None);
        let meta = std::fs::symlink_metadata(&path).expect("metadata");
        let entry = ctx
            .create_entry(&path, PathBuf::from("source.txt"), &meta)
            .expect("create_entry");

        assert_eq!(entry.permissions() & 0o7777, 0o664);
    }
}

#[cfg(all(test, unix))]
mod munge_symlinks_tests {
    //! Sender-side `munge symlinks` regression tests.
    //!
    //! upstream: flist.c:222-226 - when the daemon enabled `munge symlinks`,
    //! the sender strips the `/rsyncd-munged/` prefix after `readlink()` so
    //! the wire format carries the original target. The matching prepend on
    //! the receive side lives in `crate::receiver::directory::links`.

    use super::super::super::GeneratorContext;
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use protocol::ProtocolVersion;
    use std::ffi::OsString;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn generator_with_munge(munge_symlinks: bool) -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            munge_symlinks,
            ..Default::default()
        };
        GeneratorContext::new_for_test(&handshake, config)
    }

    #[test]
    fn sender_strips_munge_prefix_before_emitting_wire_target() {
        // upstream: flist.c:222-226 - when the daemon enabled `munge symlinks`,
        // the sender restores the original target before encoding the flist
        // entry. Verify the in-memory `FileEntry` carries the stripped path so
        // the wire format never leaks `/rsyncd-munged/`.
        let tmp = TempDir::new().unwrap();
        let link = tmp.path().join("escape");
        symlink(Path::new("/rsyncd-munged//etc/passwd"), &link).unwrap();
        let meta = std::fs::symlink_metadata(&link).unwrap();

        let ctx = generator_with_munge(true);
        let entry = ctx
            .create_entry(&link, PathBuf::from("escape"), &meta)
            .unwrap();

        assert_eq!(
            entry.link_target().map(PathBuf::as_path),
            Some(Path::new("/etc/passwd")),
            "sender must strip `/rsyncd-munged/` before transmission \
             (upstream flist.c:222-226)",
        );
    }

    #[test]
    fn sender_preserves_prefix_when_munge_disabled() {
        // Negative control: with `munge_symlinks=false` the prefix on the
        // source link is part of the user's intentional target and must
        // travel verbatim on the wire.
        let tmp = TempDir::new().unwrap();
        let link = tmp.path().join("escape");
        symlink(Path::new("/rsyncd-munged//etc/passwd"), &link).unwrap();
        let meta = std::fs::symlink_metadata(&link).unwrap();

        let ctx = generator_with_munge(false);
        let entry = ctx
            .create_entry(&link, PathBuf::from("escape"), &meta)
            .unwrap();

        assert_eq!(
            entry.link_target().map(PathBuf::as_path),
            Some(Path::new("/rsyncd-munged//etc/passwd")),
            "without `munge symlinks`, the prefix is part of the target and \
             must round-trip verbatim",
        );
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_reparse_tests {
    //! Windows reparse-point classification regression tests (WPC-8'.9).
    //!
    //! Verifies that `create_entry` routes NTFS reparse points through the
    //! WPC-8 classifier so junctions, symbolic links, and non-symlink
    //! reparse points serialise as SYMLINK-class `FileEntry` values
    //! (matching upstream's Cygwin behaviour) rather than as regular files.
    //!
    //! `mklink /j` runs without elevation on Windows 10+; `mklink /d`
    //! (directory symlink) requires admin or developer mode and is
    //! downgraded to a runtime skip when the privilege is missing, matching
    //! the in-tree integration tests that ship with the classifier itself.
    //!
    //! Mount-point coverage is left to the classifier-level integration
    //! tests because `mountvol` requires admin elevation and a free volume
    //! GUID; the wiring contract under test here is "every reparse-point
    //! attribute routes through `metadata::windows::classify_path` and lands
    //! in the symlink branch", which the junction case already verifies
    //! end-to-end.

    use super::super::super::GeneratorContext;
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use protocol::ProtocolVersion;
    use protocol::flist::FileType;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn windows_generator() -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        GeneratorContext::new_for_test(&handshake, config)
    }

    /// Returns `Some(status)` when `mklink` succeeded, `None` when the test
    /// should be skipped because `cmd.exe` is unavailable or the privilege
    /// to create the reparse point is missing.
    fn try_mklink(args: &[&str]) -> Option<()> {
        let status = Command::new("cmd").args(args).status().ok()?;
        if status.success() { Some(()) } else { None }
    }

    /// Regular file: stays Regular, does not flip to Symlink.
    ///
    /// Negative control: ensures the reparse-attribute probe does not
    /// false-positive on non-reparse entries.
    #[test]
    fn regular_file_is_classified_as_regular() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("plain.txt");
        std::fs::write(&path, b"data").expect("write file");
        let meta = std::fs::symlink_metadata(&path).expect("symlink_metadata");

        let ctx = windows_generator();
        let entry = ctx
            .create_entry(&path, PathBuf::from("plain.txt"), &meta)
            .expect("create_entry");

        assert_eq!(entry.file_type(), FileType::Regular);
        assert_eq!(entry.size(), 4);
    }

    /// Directory junction (`mklink /j`): WPC-8 classifies as `Junction`,
    /// `create_entry` emits a `Symlink` FileEntry whose target is preserved.
    ///
    /// Mirrors upstream rsync Cygwin behaviour (every reparse point becomes
    /// a POSIX symbolic link on the wire) while exercising the wiring that
    /// PR #5579 + PR #5592 deferred behind the classifier API.
    #[test]
    fn junction_is_emitted_as_symlink_entry() {
        let tmp = TempDir::new().expect("tempdir");
        let target = tmp.path().join("target");
        let junction = tmp.path().join("link");
        std::fs::create_dir(&target).expect("create target dir");

        let junction_str = match junction.to_str() {
            Some(s) => s,
            None => return, // non-UTF8 tempdir, skip
        };
        let target_str = match target.to_str() {
            Some(s) => s,
            None => return,
        };
        if try_mklink(&["/c", "mklink", "/j", junction_str, target_str]).is_none() {
            // junction creation refused (no cmd.exe, or filesystem rejects);
            // skip rather than fail so non-admin CI runners stay green.
            return;
        }

        let meta = std::fs::symlink_metadata(&junction).expect("symlink_metadata");
        let ctx = windows_generator();
        let entry = ctx
            .create_entry(&junction, PathBuf::from("link"), &meta)
            .expect("create_entry");

        assert_eq!(
            entry.file_type(),
            FileType::Symlink,
            "junction must serialise as SYMLINK for Cygwin parity"
        );
        let link_target = entry.link_target().cloned().unwrap_or_default();
        assert!(
            !link_target.as_os_str().is_empty(),
            "junction target must be preserved (got empty), \
             entry={entry:?}"
        );
    }

    /// Directory symlink (`mklink /d`): WPC-8 classifies as `Symlink`,
    /// `create_entry` emits a `Symlink` FileEntry with a non-empty target.
    ///
    /// Skipped at runtime when `mklink /d` is refused (requires admin or
    /// Windows 10 developer mode).
    #[test]
    fn directory_symlink_is_emitted_as_symlink_entry() {
        let tmp = TempDir::new().expect("tempdir");
        let target = tmp.path().join("target");
        let link = tmp.path().join("link");
        std::fs::create_dir(&target).expect("create target dir");

        let link_str = match link.to_str() {
            Some(s) => s,
            None => return,
        };
        let target_str = match target.to_str() {
            Some(s) => s,
            None => return,
        };
        if try_mklink(&["/c", "mklink", "/d", link_str, target_str]).is_none() {
            return; // privilege missing, skip
        }

        let meta = std::fs::symlink_metadata(&link).expect("symlink_metadata");
        let ctx = windows_generator();
        let entry = ctx
            .create_entry(&link, PathBuf::from("link"), &meta)
            .expect("create_entry");

        assert_eq!(entry.file_type(), FileType::Symlink);
        let link_target = entry.link_target().cloned().unwrap_or_default();
        assert!(
            !link_target.as_os_str().is_empty(),
            "directory symlink target must be preserved, entry={entry:?}"
        );
    }
}

#[cfg(all(test, unix))]
mod entry_length_tests {
    //! `F_LENGTH` parity regression for directory and symlink entries.
    //!
    //! upstream: flist.c:1465 - `file->len32 = (uint32)st.st_size` runs for
    //! every entry type; only devices and specials are zeroed at flist.c:1452
    //! and flist.c:1454. Directories therefore carry their on-disk inode size
    //! and symlinks carry `st_size` (the target byte length). That field is
    //! summed into the `--stats` "Total file size" total at flist.c:679 and
    //! rendered by `--list-only` and `%l`, so emitting 0 here would diverge
    //! from upstream's observable output byte-for-byte.

    use super::super::super::GeneratorContext;
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use protocol::ProtocolVersion;
    use protocol::flist::FileType;
    use std::ffi::OsString;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn generator() -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let mut config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        config.flags.numeric_ids = true;
        GeneratorContext::new_for_test(&handshake, config)
    }

    #[test]
    fn directory_entry_carries_on_disk_size() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path().join("subdir");
        std::fs::create_dir(&dir).expect("create dir");
        let meta = std::fs::symlink_metadata(&dir).expect("metadata");

        let ctx = generator();
        let entry = ctx
            .create_entry(&dir, PathBuf::from("subdir"), &meta)
            .expect("create_entry");

        assert_eq!(entry.file_type(), FileType::Directory);
        assert!(
            meta.len() > 0,
            "directories report a non-zero on-disk st_size on Unix",
        );
        assert_eq!(
            entry.size(),
            meta.len(),
            "directory F_LENGTH must mirror st_size (upstream flist.c:1465), \
             not the hardcoded 0 from FileEntry::new_directory",
        );
    }

    #[test]
    fn symlink_entry_carries_target_length() {
        let tmp = TempDir::new().expect("tempdir");
        let link = tmp.path().join("link");
        // lstat reports st_size == strlen(target) for symlinks.
        let target = "some/relative/target";
        symlink(target, &link).expect("create symlink");
        let meta = std::fs::symlink_metadata(&link).expect("metadata");

        let ctx = generator();
        let entry = ctx
            .create_entry(&link, PathBuf::from("link"), &meta)
            .expect("create_entry");

        assert_eq!(entry.file_type(), FileType::Symlink);
        assert_eq!(
            entry.size(),
            target.len() as u64,
            "symlink F_LENGTH must equal the target byte length \
             (upstream flist.c:1465), not the hardcoded 0 from \
             FileEntry::new_symlink",
        );
        assert_eq!(
            entry.size(),
            meta.len(),
            "F_LENGTH must mirror lstat st_size"
        );
    }
}
