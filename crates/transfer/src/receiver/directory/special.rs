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
//! - `generator.c:1627-1692` recv_generator - `FT_DEVICE` (when
//!   `preserve_devices`) and `FT_SPECIAL` (when `preserve_specials`) call
//!   `atomic_create` -> `do_mknod_at` to create the node from the flist entry.
//! - `syscall.c:do_mknod()` - the underlying `mknod(2)` (or fake-super
//!   placeholder) that materialises the node.

#[cfg(unix)]
use std::fs;
use std::path::Path;

#[cfg(unix)]
use logging::info_log;
#[cfg(unix)]
use metadata::{MetadataOptions, apply_metadata_from_file_entry, create_fifo_node_from_parts};
#[cfg(unix)]
use protocol::flist::{FileEntry, FileType};

#[cfg(unix)]
use crate::generator::ItemFlags;
use crate::receiver::ReceiverContext;
#[cfg(unix)]
use crate::receiver::directory::obstacle::MakeWayFor;

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
    /// - `generator.c:1627` - `if (preserve_devices && IS_DEVICE(file->mode))`
    /// - `generator.c:1675` - `atomic_create(file, fname, NULL, ...)`
    pub(in crate::receiver) fn create_specials<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> std::io::Result<()> {
        self.create_specials_in_range(
            0..self.file_list.len(),
            dest_dir,
            #[cfg(unix)]
            sandbox,
            writer,
        )
    }

    /// [`create_specials`](Self::create_specials) restricted to the flat-index
    /// range `[range.start, range.end)`. Upstream creates each node inline as
    /// `recv_generator()` reaches it (generator.c:2031-2060), so per-segment
    /// calls that tile the list match one whole-list call.
    #[cfg(unix)]
    pub(in crate::receiver) fn create_specials_in_range<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        range: std::ops::Range<usize>,
        dest_dir: &Path,
        sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> std::io::Result<()> {
        // upstream: generator.c:2031 `!drop_devices && ...` - --drop-D
        // withholds CREATION without touching preserve_devices /
        // preserve_specials, which also frame the file list's rdev fields.
        // Entries fall through to the non-regular skip path exactly as they do
        // when -D was never given.
        if self.config.flags.skip_dest_writes()
            || self.config.flags.drop_devices
            || (!self.config.flags.devices && !self.config.flags.specials)
        {
            return Ok(());
        }

        let start = range.start;
        for (i, entry) in self.file_list[range].iter().enumerate() {
            let flist_idx = start + i;
            let is_device = entry.is_device();
            let is_special = entry.is_special();
            if is_device {
                if !self.config.flags.devices {
                    continue;
                }
                // upstream: generator.c:2032 - the device arm is
                // `am_root && preserve_devices && ftype == FT_DEVICE`. Without
                // the privilege term an unprivileged receiver enters the
                // creation branch, unlinks whatever occupies the destination,
                // and only then fails the `mknod` it was never able to
                // perform - destroying an existing file. Upstream instead
                // falls through to the non-regular skip at generator.c:2109,
                // leaving the destination untouched.
                //
                // The term is `am_root != 0`, so `--fake-super` qualifies: it
                // is the -1 arm of the tri-state and still writes the 0600
                // placeholder (`syscall.c:do_mknod()`).
                if !metadata::am_root() && !self.config.fake_super {
                    // upstream: generator.c:2109-2114 - INFO_GTE(NONREG, 1),
                    // which is info_verbosity[0] and so prints at default
                    // verbosity.
                    info_log!(
                        Nonreg,
                        1,
                        "skipping non-regular file \"{}\"",
                        entry.path().display()
                    );
                    continue;
                }
            } else if is_special {
                if !self.config.flags.specials {
                    continue;
                }
            } else {
                continue;
            }

            let relative_path = entry.path();
            let node_path = dest_dir.join(relative_path);

            // Ensure parent directory exists for --relative paths.
            // upstream: generator.c:1329-1338 make_path() for relative_paths
            if let Some(parent) = node_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // upstream: generator.c:1651-1670 - an existing node of the same
            // type (and, for devices, the same rdev) is treated as up-to-date;
            // only its metadata is refreshed.
            let up_to_date = existing_special_matches(&node_path, entry, is_device);

            // Whether the destination already existed before this create. Only a
            // truly absent destination is ITEM_IS_NEW and bumps
            // stats.created_devices / stats.created_specials; a same-type
            // up-to-date node or a replaced wrong-type obstacle is not a
            // creation (upstream generator.c:1651-1670, `statret < 0`). Probed
            // before the obstacle unlink below so a replacement is not
            // misclassified as new.
            let mut dest_existed = up_to_date;

            if !up_to_date {
                dest_existed = fs::symlink_metadata(&node_path).is_ok();
                // upstream: generator.c:1679 - `else if (basis_dir[0] != NULL)`
                // is reached only when the destination is absent (`statret !=
                // 0`). An identical node in a `--compare-dest` basis leaves the
                // destination absent; a `--link-dest` basis is hard-linked. A
                // wrong-type obstacle (dest present) skips the basis lookup and
                // is replaced below, matching upstream's `statret == 0` branch.
                if !self.config.reference_directories.is_empty()
                    && fs::symlink_metadata(&node_path).is_err()
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

                // upstream: generator.c:2091 atomic_create(..., del_for_flag) -
                // one decision for the obstacle: rmdir a directory, back up or
                // unlink anything else. The refusal is reported inside, so a
                // blocked entry no longer exits 0 in silence.
                //
                // upstream: generator.c:2041-2047 - the noun in the refusal
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
                // upstream: generator.c:2506-2521 - the S_ISSOCK EOPNOTSUPP arm
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

                // upstream: generator.c:1675 atomic_create -> do_mknod_at.
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
                    // upstream: generator.c:2521-2522 atomic_create() -
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

            // upstream: generator.c:1672 set_file_attrs(fname, file, ...) runs
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

            if up_to_date {
                // upstream: generator.c:1145 - "%s is uptodate" at INFO_GTE(NAME, 2)
                let iflags = ItemFlags::from_raw(0);
                let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, entry);
                self.record_server_no_transfer_itemize(flist_idx, iflags.raw());
                info_log!(Name, 2, "{} is uptodate", relative_path.display());
            } else {
                // upstream: generator.c:1462 itemize() sets ITEM_IS_NEW when the
                // receiver newly materialises the node via do_mknod().
                let iflags =
                    ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
                let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, entry);
                self.record_server_no_transfer_itemize(flist_idx, iflags.raw());
                if !dest_existed {
                    // upstream: receiver.c:759-762 - a newly created device
                    // (created_devices) or FIFO/socket (created_specials),
                    // classified by mode.
                    self.record_created(entry.mode());
                }
            }
        }
        Ok(())
    }

    /// Skip-with-warning on non-Unix platforms. Native (non-Cygwin) Windows has
    /// no `mknod`, `mkfifo`, or `AF_UNIX` bind, so a device, FIFO, or socket
    /// entry in the file list cannot be materialised. Rather than silently
    /// dropping the entry or aborting the whole transfer, emit one warning per
    /// skipped entry and leave the destination untouched, per the WIND-2
    /// contract in `docs/user/windows-support-matrix.md`.
    #[cfg(not(unix))]
    pub(in crate::receiver) fn create_specials_in_range<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        range: std::ops::Range<usize>,
        _dest_dir: &Path,
        _writer: &mut W,
    ) -> std::io::Result<()> {
        // upstream: generator.c:2031 - see the unix arm above.
        if self.config.flags.skip_dest_writes()
            || self.config.flags.drop_devices
            || (!self.config.flags.devices && !self.config.flags.specials)
        {
            return Ok(());
        }

        for entry in &self.file_list[range] {
            let gated = (entry.is_device() && self.config.flags.devices)
                || (entry.is_special() && self.config.flags.specials);
            if !gated {
                continue;
            }
            logging::info_log!(
                Nonreg,
                1,
                "skipping special file \"{}\": device and special files are not supported on this platform",
                entry.path().display()
            );
        }
        Ok(())
    }
}

/// Returns `true` when an on-disk node at `path` already matches the wire
/// entry: same node type, and for devices the same rdev. Any read failure
/// (including a missing path) reports `false` so the caller (re)creates it.
///
/// upstream: generator.c:1651-1670 - the receiver's quick-check leaves a
/// matching special/device node in place instead of recreating it.
#[cfg(unix)]
fn existing_special_matches(path: &Path, entry: &FileEntry, is_device: bool) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    let file_type = meta.file_type();

    if is_device {
        let type_ok = if entry.is_block_device() {
            file_type.is_block_device()
        } else {
            file_type.is_char_device()
        };
        type_ok
            && meta.rdev()
                == metadata::device_word(
                    entry.rdev_major().unwrap_or(0),
                    entry.rdev_minor().unwrap_or(0),
                )
    } else if entry.file_type() == FileType::Socket {
        file_type.is_socket()
    } else {
        file_type.is_fifo()
    }
}
