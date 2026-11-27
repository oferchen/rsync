//! Special-file (FIFO, socket, device node) creation from the received file list.
//!
//! The protocol receiver materialises FIFOs, Unix-domain sockets, and
//! character/block device nodes named in the file list, gated on the
//! transfer's `--specials` / `--devices` flags. Without this pass the receiver
//! silently drops every special entry: the flist carries them, but only
//! regular files, directories, and symlinks reach disk, so a
//! `rsync -a remote:src/ dst/` pull (or a push to an oc-rsync daemon) loses
//! fifos and devices with no error and a zero exit.
//!
//! Runs as a first pass alongside `create_symlinks`, before the per-file data
//! loop, mirroring upstream's generator which materialises the node from the
//! flist entry rather than transferring any payload.
//!
//! # Upstream Reference
//!
//! - `generator.c:2031-2107` `recv_generator()` - `FT_DEVICE` (when `am_root`
//!   and `preserve_devices`) and `FT_SPECIAL` (when `preserve_specials`) call
//!   `atomic_create` -> `do_mknod_at` to create the node from the flist entry.
//! - `syscall.c:do_mknod()` - the underlying `mknod(2)` (or fake-super
//!   placeholder) that materialises the node.

use std::path::Path;

#[cfg(unix)]
use crate::disk_commit::LiveAclCache;
#[cfg(unix)]
use logging::info_log;
#[cfg(unix)]
use metadata::{
    AclIdMapper, MetadataOptions, apply_metadata_from_file_entry, create_fifo_node_from_parts,
};
use protocol::flist::FileEntry;
#[cfg(unix)]
use protocol::flist::FileType;

#[cfg(unix)]
use crate::generator::ItemFlags;
use crate::receiver::ReceiverContext;
#[cfg(unix)]
use crate::receiver::apply_acls_from_receiver_cache;
#[cfg(unix)]
use crate::receiver::directory::obstacle::MakeWayFor;
#[cfg(unix)]
use crate::receiver::transfer::Settled;

impl ReceiverContext {
    /// Creates FIFO, socket, and device nodes from the file list entries.
    ///
    /// Devices are gated on `--devices`, FIFOs and sockets on `--specials`
    /// (matching upstream's `preserve_devices` / `preserve_specials`). An
    /// existing node of the same type - and, for devices, the same rdev - is
    /// left in place and only refreshed; any other obstacle is removed so the
    /// fresh node can be created. Fake-super substitutes a `0600` placeholder
    /// for the node, mirroring `syscall.c:do_mknod()`'s `am_root < 0` branch.
    ///
    /// A per-entry creation failure is reported as `FERROR_XFER` and skipped
    /// rather than aborting the transfer, so the run ends `RERR_PARTIAL` (23)
    /// like upstream's `atomic_create()` `mknod %s failed` path.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2031-2033` - `!drop_devices && ((am_root && preserve_devices &&
    ///   ftype == FT_DEVICE) || (preserve_specials && ftype == FT_SPECIAL))`
    /// - `generator.c:2091` - `atomic_create(file, fname, NULL, NULL, rdev, ...)`
    pub(in crate::receiver) fn create_specials<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] acl_cache: Option<&LiveAclCache>,
        #[cfg(unix)] acl_id_map: Option<&AclIdMapper>,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> std::io::Result<()> {
        self.create_specials_in_range(
            0..self.file_list.len(),
            dest_dir,
            #[cfg(unix)]
            acl_cache,
            #[cfg(unix)]
            acl_id_map,
            #[cfg(unix)]
            sandbox,
            writer,
        )
    }

    /// [`create_specials`](Self::create_specials) restricted to the flat-index
    /// range `[range.start, range.end)`. Upstream creates each node inline as
    /// `recv_generator()` reaches it (generator.c:2031-2107), so per-segment
    /// calls that tile the list match one whole-list call.
    #[cfg(unix)]
    pub(in crate::receiver) fn create_specials_in_range<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        range: std::ops::Range<usize>,
        dest_dir: &Path,
        acl_cache: Option<&LiveAclCache>,
        acl_id_map: Option<&AclIdMapper>,
        sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> std::io::Result<()> {
        // upstream: generator.c:2072 `!drop_devices && ...` - --drop-D
        // withholds CREATION without touching preserve_devices /
        // preserve_specials, which also frame the file list's rdev fields.
        // Entries fall through to the non-regular skip path exactly as they do
        // when -D was never given.
        let creates = !self.config.flags.skip_dest_writes()
            && !self.config.flags.drop_devices
            && (self.config.flags.devices || self.config.flags.specials);
        if !creates && !self.may_skip_non_regular() {
            return Ok(());
        }

        let mut daemon_filters = self.daemon_filter_gate(dest_dir);
        let start = range.start;
        for (i, entry) in self.file_list[range].iter().enumerate() {
            let flist_idx = start + i;
            let skipped = self.skips_as_non_regular(entry);
            if !skipped && !(creates && (entry.is_device() || entry.is_special())) {
                continue;
            }
            // upstream: generator.c:1699-1713 - the daemon filter is checked
            // before the existence gate and the device/special branch.
            if self.daemon_refuses_non_dir(daemon_filters.as_mut(), writer, entry) {
                continue;
            }
            // upstream: generator.c:1794-1843 - the existence gate precedes
            // both the device/special branch and the non-regular skip.
            if self.skip_non_regular_by_existence_gate(
                &mut *writer,
                entry,
                dest_dir,
                &dest_dir.join(entry.path()),
            ) {
                continue;
            }
            if skipped {
                self.report_skipped_non_regular(entry);
                continue;
            }
            let is_device = entry.is_device();

            // upstream: generator.c:1980-1982 - a follower is handed to
            // hard_link_check() before the FT_SPECIAL/FT_DEVICE branch, so it
            // is linked to its leader (create_hardlinks) and never mknod'ed.
            if self.config.flags.hard_links
                && crate::receiver::quick_check::is_hardlink_follower(entry)
            {
                continue;
            }

            let relative_path = entry.path();
            let node_path = dest_dir.join(relative_path);

            // Ensure parent directory exists for --relative paths.
            // upstream: generator.c:1753-1763 make_path() for relative_paths
            if let Some(parent) = node_path.parent() {
                let _ = super::dest_create_dir_all(sandbox, dest_dir, parent);
            }

            // upstream: generator.c:2089-2104 - the destination is read through
            // x_lstat(), so a --fake-super placeholder answers with its %stat
            // type and rdev.
            let existing =
                existing_special_stat(sandbox, dest_dir, &node_path, self.config.fake_super)
                    .map_err(|error| {
                        let name = self.full_fname_in_dest(dest_dir, &node_path);
                        metadata::rename_corrupt_fake_super(error, name)
                    })?;
            let disposition = SpecialDisposition::of(existing.as_ref(), entry);
            let up_to_date = disposition == SpecialDisposition::UpToDate;
            // upstream: generator.c:2096/2094 - itemize() compares the stat
            // taken before set_file_attrs() refreshes it.
            let iflags = match disposition {
                SpecialDisposition::UpToDate => {
                    self.itemize_existing_flags(entry, &node_path, existing.as_ref(), 0)
                }
                SpecialDisposition::Recreate => self.itemize_existing_flags(
                    entry,
                    &node_path,
                    existing.as_ref(),
                    ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_REPORT_CHANGE,
                ),
                // upstream: generator.c:2135-2136 - the base flags are the same
                // for a new node; itemize() adds ITEM_IS_NEW for statret < 0.
                SpecialDisposition::Create => {
                    ItemFlags::ITEM_LOCAL_CHANGE
                        | ItemFlags::ITEM_REPORT_CHANGE
                        | ItemFlags::ITEM_IS_NEW
                }
            };
            let iflags = ItemFlags::from_raw(iflags);

            // Whether the destination already existed before this create. Only a
            // truly absent destination is ITEM_IS_NEW and bumps
            // stats.created_devices / stats.created_specials; a same-type
            // up-to-date node or a replaced wrong-type obstacle is not a
            // creation (upstream generator.c:2080 `dest_existed`). Probed
            // before the obstacle unlink below so a replacement is not
            // misclassified as new.
            let mut dest_existed = up_to_date;

            if !up_to_date {
                dest_existed = super::dest_entry_exists(sandbox, dest_dir, &node_path);
                // upstream: generator.c:2105 - `else if (basis_dir[0] != NULL)`
                // is reached only when the destination is absent (`statret !=
                // 0`). An identical node in a `--compare-dest` basis leaves the
                // destination absent; a `--link-dest` basis is hard-linked. A
                // wrong-type obstacle (dest present) skips the basis lookup and
                // is replaced below, matching upstream's `statret == 0` branch.
                if !self.config.reference_directories.is_empty()
                    && !super::dest_entry_exists(sandbox, dest_dir, &node_path)
                {
                    let basis = if is_device {
                        crate::receiver::quick_check::NonRegularBasis::Device {
                            rdev: metadata::device_word(
                                entry.rdev_major().unwrap_or(0),
                                entry.rdev_minor().unwrap_or(0),
                            ),
                        }
                    } else {
                        crate::receiver::quick_check::NonRegularBasis::Special {
                            is_socket: entry.file_type() == FileType::Socket,
                        }
                    };
                    let compare_opts = MetadataOptions::new()
                        .preserve_permissions(self.config.flags.perms)
                        .preserve_owner(self.config.flags.owner)
                        .preserve_group(self.config.flags.group)
                        .preserve_times(self.config.flags.times)
                        .preserve_atimes(self.config.flags.atimes)
                        .preserve_crtimes(self.config.flags.crtimes)
                        .numeric_ids(self.config.flags.numeric_ids.maps_numeric())
                        .fake_super(self.config.fake_super);
                    if crate::receiver::quick_check::try_reference_dest_non(
                        entry,
                        dest_dir,
                        &self.config.reference_directories,
                        &basis,
                        &compare_opts,
                        self.config.file_selection.modify_window,
                    ) {
                        continue;
                    }
                }

                // upstream: generator.c:2132 atomic_create(..., del_for_flag) -
                // one decision for the obstacle: rmdir a directory, back up or
                // unlink anything else. The refusal is reported inside, so a
                // blocked entry no longer exits 0 in silence.
                //
                // upstream: generator.c:2081-2088 - the noun in the refusal
                // comes from the NEW entry's type, DEL_FOR_DEVICE for a device
                // and DEL_FOR_SPECIAL for a FIFO or socket.
                let make_way_for = if is_device {
                    MakeWayFor::Device
                } else {
                    MakeWayFor::Special
                };
                if self
                    .make_way_for_replacement(
                        writer,
                        &node_path,
                        relative_path,
                        dest_dir,
                        sandbox,
                        make_way_for,
                    )
                    .is_err()
                {
                    continue;
                }

                // A socket the platform cannot create race-safely is SKIPPED
                // with a warning rather than materialised through an
                // unconfined path-based bind; the inode is only a placeholder.
                // upstream: generator.c:2551-2562 - the S_ISSOCK EOPNOTSUPP arm
                if entry.file_type() == FileType::Socket
                    && metadata::socket_creation_unsupported(relative_path)
                {
                    // upstream: log.c:rwrite() - a server frames FWARNING as
                    // MSG_WARNING for the client, so a push reports it too.
                    let _ = self.emit_warning_line(
                        writer,
                        &format!(
                            "skipping socket (creation unsupported here): {}\n",
                            self.full_fname_in_dest(dest_dir, &node_path)
                        ),
                    );
                    continue;
                }

                // upstream: generator.c:2132 atomic_create -> do_mknod_at.
                // FIFO and device nodes are materialised through the confined
                // parent dirfd so a raced parent-component symlink swap cannot
                // redirect the new node outside the transfer root
                // (do_mknod_at's secure_relpath arm). The socket arm keeps the
                // existing path-based create: a nested socket has already been
                // skipped above (`socket_creation_unsupported`, no
                // dirfd-relative `bind(2)`) so only a top-level one reaches
                // here, and confining the Linux `mknod(S_IFSOCK)` is the
                // documented residual.
                let create_result: std::io::Result<()> = if is_device {
                    let mode =
                        metadata::device_mknod_mode(entry.mode() & 0o7777, entry.is_block_device());
                    let dev = metadata::device_word(
                        entry.rdev_major().unwrap_or(0),
                        entry.rdev_minor().unwrap_or(0),
                    );
                    fast_io::mknodat_via_sandbox_or_fallback(
                        sandbox,
                        dest_dir,
                        relative_path,
                        &node_path,
                        mode,
                        dev,
                        self.config.fake_super,
                    )
                } else if entry.file_type() == FileType::Socket {
                    create_fifo_node_from_parts(
                        &node_path,
                        entry.mode() & 0o7777,
                        true,
                        self.config.fake_super,
                    )
                    .map_err(|error| error.into_parts().2)
                } else {
                    let mode = metadata::fifo_mknod_mode(entry.mode() & 0o7777);
                    fast_io::mknodat_via_sandbox_or_fallback(
                        sandbox,
                        dest_dir,
                        relative_path,
                        &node_path,
                        mode,
                        0,
                        self.config.fake_super,
                    )
                };
                if let Err(error) = create_result {
                    // upstream: generator.c:2564-2565 atomic_create() -
                    // rsyserr(FERROR_XFER, e, "mknod %s failed",
                    // full_fname(create_name)). FERROR_XFER sets
                    // got_xfer_error (log.c:337-338), which lifts the exit to
                    // RERR_PARTIAL (23); the entry is skipped, the run goes on.
                    let _ = self.emit_generator_error_xfer(
                        writer,
                        &format!(
                            "mknod {} failed",
                            self.full_fname_in_dest(dest_dir, &node_path)
                        ),
                        &error,
                    );
                    continue;
                }
            }

            // upstream: generator.c:2094/2092 set_file_attrs(fname, file, ...) runs
            // for both the freshly-created and up-to-date branches so the node
            // carries the sender-supplied perms/owner/times.
            let options = MetadataOptions::new()
                .preserve_permissions(self.config.flags.perms)
                .preserve_owner(self.config.flags.owner)
                .preserve_group(self.config.flags.group)
                .preserve_times(self.config.flags.times)
                .preserve_atimes(self.config.flags.atimes)
                .preserve_crtimes(self.config.flags.crtimes)
                .numeric_ids(self.config.flags.numeric_ids.maps_numeric())
                .fake_super(self.config.fake_super);
            // upstream: rsync.c:set_file_attrs() - a failed chown/utimes/chmod
            // is rsyserr(FERROR_XFER), so the run ends RERR_PARTIAL (23).
            if let Err(error) = apply_metadata_from_file_entry(&node_path, entry, &options) {
                let _ = self.emit_generator_attrs_failure(writer, dest_dir, &error);
            }
            // upstream: rsync.c:800 set_file_attrs() -> set_acl() for every
            // non-symlink entry, devices and specials included; a FIFO is
            // pinned with O_NONBLOCK and a device or socket is addressed
            // through its parent (rsync.c:563-571, acls.c:1315-1321).
            if let Err(error) = apply_acls_from_receiver_cache(
                &node_path,
                entry,
                acl_cache,
                acl_id_map,
                true,
                Some(dest_dir),
                self.config.fake_super,
            ) {
                let _ = self.emit_generator_attrs_failure(writer, dest_dir, &error);
            }

            if up_to_date {
                // upstream: rsync.c:823-828 set_file_attrs() - "%s is uptodate" at
                // INFO_GTE(NAME, 2)
                let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, entry);
                self.record_no_transfer_itemize(flist_idx, iflags.raw());
                info_log!(Name, 2, "{} is uptodate", relative_path.display());
            } else {
                let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, entry);
                self.record_no_transfer_itemize(flist_idx, iflags.raw());
                if !dest_existed {
                    // upstream: receiver.c:925-938 - a newly created device
                    // (created_devices) or FIFO/socket (created_specials),
                    // classified by mode.
                    self.record_created(entry.mode());
                }
            }
            // upstream: generator.c:2101-2102 (identical) and :2144-2145 (made)
            self.send_generator_success(writer, flist_idx, Settled::InPlace)?;
        }
        Ok(())
    }

    /// Skip-with-warning on non-Unix platforms.
    ///
    /// Deviation from upstream: native (non-Cygwin) Windows has
    /// no `mknod`, `mkfifo`, or `AF_UNIX` bind, so a device, FIFO, or socket
    /// entry in the file list cannot be materialised. Rather than silently
    /// dropping the entry or aborting the whole transfer, emit one warning per
    /// skipped entry and leave the destination untouched, per the
    /// contract in `docs/user/windows-support-matrix.md`.
    #[cfg(not(unix))]
    pub(in crate::receiver) fn create_specials_in_range<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        range: std::ops::Range<usize>,
        dest_dir: &Path,
        writer: &mut W,
    ) -> std::io::Result<()> {
        // upstream: generator.c:2072 - see the unix arm above.
        let creates = !self.config.flags.skip_dest_writes()
            && !self.config.flags.drop_devices
            && (self.config.flags.devices || self.config.flags.specials);
        if !creates && !self.may_skip_non_regular() {
            return Ok(());
        }

        let mut daemon_filters = self.daemon_filter_gate(dest_dir);
        for entry in &self.file_list[range] {
            let gated = (entry.is_device() && self.config.flags.devices)
                || (entry.is_special() && self.config.flags.specials);
            if creates && gated {
                logging::info_log!(
                    Nonreg,
                    1,
                    "skipping special file \"{}\": device and special files are not supported on this platform",
                    entry.path().display()
                );
                continue;
            }
            if !self.skips_as_non_regular(entry)
                || self.daemon_refuses_non_dir(daemon_filters.as_mut(), writer, entry)
                || self.skip_non_regular_by_existence_gate(
                    &mut *writer,
                    entry,
                    dest_dir,
                    &dest_dir.join(entry.path()),
                )
            {
                continue;
            }
            self.report_skipped_non_regular(entry);
        }
        Ok(())
    }

    /// Whether any file-list entry can reach the non-regular skip, so a
    /// transfer that preserves every non-regular type skips the pass.
    fn may_skip_non_regular(&self) -> bool {
        let flags = &self.config.flags;
        !flags.links
            || flags.drop_devices
            || !flags.devices
            || !flags.specials
            || !self.may_create_devices()
    }

    /// Whether `recv_generator()` falls through to the non-regular skip for
    /// `entry`: a symlink without `--links`, a device without `--devices` or
    /// without the privilege to create it, or a FIFO/socket without
    /// `--specials`. `--drop-D` sends devices and specials the same way.
    ///
    /// upstream: generator.c:1985 `preserve_links && ftype == FT_SYMLINK`,
    /// generator.c:2072-2074 `!drop_devices && ((am_root && preserve_devices
    /// && ftype == FT_DEVICE) || (preserve_specials && ftype == FT_SPECIAL))`,
    /// and generator.c:2150 `if (ftype != FT_REG)`.
    fn skips_as_non_regular(&self, entry: &FileEntry) -> bool {
        let flags = &self.config.flags;
        if entry.is_symlink() {
            !flags.links
        } else if entry.is_device() {
            flags.drop_devices || !flags.devices || !self.may_create_devices()
        } else if entry.is_special() {
            flags.drop_devices || !flags.specials
        } else {
            false
        }
    }

    /// The `am_root` term of the device arm (generator.c:2073). Without it an
    /// unprivileged receiver would enter the creation branch, unlink whatever
    /// occupies the destination, and only then fail the `mknod` it could never
    /// perform. `am_root` is non-zero under `--fake-super` (the -1 arm of the
    /// tri-state), which still writes the 0600 placeholder
    /// (`syscall.c:do_mknod()`).
    fn may_create_devices(&self) -> bool {
        metadata::am_root() || self.config.fake_super
    }

    /// Reports an entry the receiver leaves uncreated.
    ///
    /// upstream: generator.c:2150-2155 - `INFO_GTE(NONREG, 1)`, which is
    /// info_verbosity[0] (options.c:257) and so prints at default verbosity.
    /// A lone entry the single-file rename wrote under the destination
    /// operand's name is still named by its file-list name: generator.c:2152-2153
    /// swaps `fname` for `f_name(file, NULL)` under `solo_file`.
    fn report_skipped_non_regular(&self, entry: &FileEntry) {
        logging::info_log_bytes!(Nonreg, 1, {
            let name = self.single_file_renamed_from.as_deref().map_or_else(
                || entry.name_bytes().into_owned(),
                super::deletion::path_wire_bytes,
            );
            let mut message = b"skipping non-regular file \"".to_vec();
            message.extend_from_slice(&name);
            message.push(b'"');
            message
        });
    }
}

/// The destination as upstream's generator reads it: `x_lstat()` folds a
/// `--fake-super` placeholder's `%stat` record into its stat. A destination
/// that cannot be stat'ed reads as absent.
///
/// # Errors
///
/// A `%stat` record that does not parse, which upstream treats as fatal.
///
/// upstream: xattrs.c:1372-1377 x_lstat()
#[cfg(unix)]
fn existing_special_stat(
    sandbox: Option<&fast_io::DirSandbox>,
    dest_dir: &Path,
    path: &Path,
    fake_super: bool,
) -> std::io::Result<Option<fast_io::EntryMetadata>> {
    let Ok(stat) = super::dest_lstat(sandbox, dest_dir, path, false) else {
        return Ok(None);
    };
    if fake_super {
        metadata::fold_fake_super_stat(path, stat).map(Some)
    } else {
        Ok(Some(stat))
    }
}

/// What the generator does with a device or special whose destination it
/// has stat'ed.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpecialDisposition {
    /// Same file type and `quick_check_ok()`: only the attributes are set.
    UpToDate,
    /// Same file type that fails `quick_check_ok()`: recreated in place.
    Recreate,
    /// Absent, or another file type (`statret = -1`): created as new.
    Create,
}

#[cfg(unix)]
impl SpecialDisposition {
    /// upstream: generator.c:2089-2093 compares `ftype` with `stype`, then
    /// quick_check_ok() (generator.c:670-680) compares `st_rdev` for a device
    /// and the `_S_IFMT` bits for a special.
    fn of(existing: Option<&fast_io::EntryMetadata>, entry: &FileEntry) -> Self {
        const S_IFMT: u32 = 0o170_000;
        let Some(existing) = existing else {
            return Self::Create;
        };
        let wanted = entry.mode() & S_IFMT;
        let found = existing.mode() & S_IFMT;
        let quick_check_ok = match special_kind(wanted) {
            Some(SpecialKind::Device) => {
                existing.rdev()
                    == metadata::device_word(
                        entry.rdev_major().unwrap_or(0),
                        entry.rdev_minor().unwrap_or(0),
                    )
            }
            Some(SpecialKind::Special) => found == wanted,
            None => false,
        };
        if special_kind(found) != special_kind(wanted) {
            Self::Create
        } else if quick_check_ok {
            Self::UpToDate
        } else {
            Self::Recreate
        }
    }
}

/// Upstream's `FT_DEVICE` and `FT_SPECIAL` buckets.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpecialKind {
    Device,
    Special,
}

/// upstream: generator.c:651-664 get_file_type() - block and character devices share
/// `FT_DEVICE`, FIFOs and sockets share `FT_SPECIAL`.
#[cfg(unix)]
const fn special_kind(file_type: u32) -> Option<SpecialKind> {
    match file_type {
        0o060_000 | 0o020_000 => Some(SpecialKind::Device),
        0o010_000 | 0o140_000 => Some(SpecialKind::Special),
        _ => None,
    }
}
