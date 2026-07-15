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
use logging::{debug_log, info_log};
#[cfg(unix)]
use metadata::{
    MetadataOptions, apply_metadata_from_file_entry, create_device_node_from_parts,
    create_fifo_node_from_parts,
};
#[cfg(unix)]
use protocol::flist::{FileEntry, FileType};

#[cfg(unix)]
use crate::generator::ItemFlags;
use crate::receiver::ReceiverContext;

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
    /// A per-entry creation failure is logged and skipped rather than aborting
    /// the transfer, mirroring upstream's `do_mknod` failure path which records
    /// an I/O error and continues with the next entry.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1627` - `if (preserve_devices && IS_DEVICE(file->mode))`
    /// - `generator.c:1663` - `atomic_create(file, fname, NULL, ...)`
    #[cfg(unix)]
    pub(in crate::receiver) fn create_specials<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> std::io::Result<()> {
        if self.config.flags.skip_dest_writes()
            || (!self.config.flags.devices && !self.config.flags.specials)
        {
            return Ok(());
        }

        for entry in &self.file_list {
            let is_device = entry.is_device();
            let is_special = entry.is_special();
            if is_device {
                if !self.config.flags.devices {
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
            // upstream: generator.c:1317-1326 make_path() for relative_paths
            if let Some(parent) = node_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // upstream: generator.c:1651-1670 - an existing node of the same
            // type (and, for devices, the same rdev) is treated as up-to-date;
            // only its metadata is refreshed.
            let up_to_date = existing_special_matches(&node_path, entry, is_device);

            if !up_to_date {
                // upstream: generator.c:2018-2020 atomic_create - when --backup
                // is set and an existing item is being replaced, preserve it to
                // the backup location before it is removed. On backup-mechanism
                // failure upstream returns 0 from atomic_create (skips the
                // entry); mirror that by logging and continuing.
                match self.backup_existing_before_replace(
                    &node_path,
                    relative_path,
                    dest_dir,
                    sandbox,
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        // SEC-1.g: route the obstacle unlink through the sandbox
                        // dirfd when the destination parent is the sandbox root
                        // so a TOCTOU swap between the stat above and this
                        // unlink cannot redirect the syscall. Falls back to
                        // path-based removal otherwise. The result is
                        // intentionally ignored: a dangling obstacle simply
                        // surfaces as a create failure below.
                        let _ = fast_io::unlink_via_sandbox_or_fallback(
                            sandbox,
                            dest_dir,
                            relative_path,
                            &node_path,
                            fast_io::UnlinkFlags::File,
                        );
                    }
                    Err(error) => {
                        debug_log!(
                            Recv,
                            1,
                            "failed to back up existing special file {}: {}",
                            node_path.display(),
                            error
                        );
                        continue;
                    }
                }

                // upstream: generator.c:1663 atomic_create -> do_mknod_at
                let create_result = if is_device {
                    create_device_node_from_parts(
                        &node_path,
                        entry.mode() & 0o7777,
                        entry.is_block_device(),
                        entry.rdev_major().unwrap_or(0),
                        entry.rdev_minor().unwrap_or(0),
                        self.config.fake_super,
                    )
                } else {
                    create_fifo_node_from_parts(
                        &node_path,
                        entry.mode() & 0o7777,
                        entry.file_type() == FileType::Socket,
                        self.config.fake_super,
                    )
                };
                if let Err(error) = create_result {
                    // upstream: generator.c do_mknod failure - rsyserr() then
                    // io_error |= IOERR_GENERAL and continue with the next
                    // entry rather than aborting the whole transfer.
                    debug_log!(
                        Recv,
                        1,
                        "failed to create special file {}: {}",
                        node_path.display(),
                        error
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
            if let Err(error) = apply_metadata_from_file_entry(&node_path, entry, &options) {
                debug_log!(
                    Recv,
                    1,
                    "failed to apply metadata for special file {}: {}",
                    node_path.display(),
                    error
                );
            }

            if up_to_date {
                // upstream: generator.c:1133 - "%s is uptodate" at INFO_GTE(NAME, 2)
                let iflags = ItemFlags::from_raw(0);
                let _ = self.emit_itemize(writer, &iflags, entry);
                info_log!(Name, 2, "{} is uptodate", relative_path.display());
            } else {
                // upstream: generator.c:1462 itemize() sets ITEM_IS_NEW when the
                // receiver newly materialises the node via do_mknod().
                let iflags =
                    ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
                let _ = self.emit_itemize(writer, &iflags, entry);
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
    pub(in crate::receiver) fn create_specials<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        _dest_dir: &Path,
        _writer: &mut W,
    ) -> std::io::Result<()> {
        if self.config.flags.skip_dest_writes()
            || (!self.config.flags.devices && !self.config.flags.specials)
        {
            return Ok(());
        }

        for entry in &self.file_list {
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
