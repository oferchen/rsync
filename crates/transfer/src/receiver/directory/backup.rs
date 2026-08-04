//! Backup of an existing non-regular destination entry before it is replaced.
//!
//! When `--backup` is set and the receiver is about to replace an existing
//! symlink, FIFO, socket, or device node with a fresh one from the file list,
//! upstream's generator preserves the old entry to the backup location before
//! creating the replacement. Without this pass oc-rsync would unlink the old
//! entry outright, silently losing the user's prior data.
//!
//! # Upstream Reference
//!
//! - `generator.c:2005-2026` `atomic_create()` - for a symlink/device/special
//!   replacing an existing item (`del_for_flag`), calls `make_backup(fname, ...)`
//!   before creating the new node; only when `--backup` is off does it fall
//!   through to `delete_item()`.
//! - `backup.c:187-221` `link_or_rename()` - hard-links the existing item into
//!   the backup area (the `DEBUG_GTE(BACKUP, 1)` "HLINK" success line), falling
//!   back to `rename(2)` ("RENAME") when the link cannot be made (cross-device
//!   or a filesystem/type that cannot be hard-linked). Upstream's default
//!   `atomic_create` (no `--temp-dir`) passes `prefer_rename = 0`, so the
//!   hard-link path is tried first for symlinks and specials on platforms that
//!   define `CAN_HARDLINK_SYMLINK` / `CAN_HARDLINK_SPECIAL`.
//! - `backup.c:352-353` - `INFO_GTE(BACKUP, 1)` "backed up %s to %s".

#[cfg(any(unix, windows))]
use std::ffi::OsStr;
#[cfg(any(unix, windows))]
use std::fs;
#[cfg(any(unix, windows))]
use std::io;
#[cfg(any(unix, windows))]
use std::path::Path;

#[cfg(any(unix, windows))]
use metadata::MetadataOptions;

#[cfg(any(unix, windows))]
use crate::receiver::ReceiverContext;

/// Which mechanism moved the existing entry into the backup area.
#[cfg(any(unix, windows))]
enum BackupPlacement {
    /// Hard-linked (upstream `link_or_rename` "HLINK" branch). The original
    /// still exists as a second link and must be unlinked by the caller path
    /// (done inside the method) before the replacement node is created.
    Hardlinked,
    /// Renamed (upstream "RENAME" fallback). The original path is now free.
    Renamed,
    /// Recreated as a symlink on a different filesystem after both the
    /// hard-link and the rename failed cross-device (upstream copy tier
    /// "SYMLINK" branch, `backup.c:296-300`). The original was already
    /// unlinked while recreating, so the path is free.
    // Only the `#[cfg(unix)]` cross-device copy tier constructs this; the
    // variant stays visible so `report_backup`/placement matches compile on
    // every platform.
    #[cfg_attr(not(unix), allow(dead_code))]
    CopiedSymlink,
    /// Recreated as a FIFO, socket, or device node on a different filesystem
    /// after both the hard-link and the rename failed cross-device (upstream
    /// copy tier "DEVICE" branch, `backup.c:288-291`). The original was
    /// already unlinked while recreating, so the path is free.
    #[cfg_attr(not(unix), allow(dead_code))]
    CopiedNode,
}

/// Places `existing` into `backup_path`, mirroring upstream `link_or_rename`.
///
/// With `prefer_rename = false` the hard-link tier runs first and the rename is
/// the fallback; with `prefer_rename = true` the whole link tier is skipped and
/// the entry is renamed straight into the backup area.
///
/// upstream: `backup.c:191` - `if (!prefer_rename)` gates the entire
/// `do_link_at` tier, so `make_backup(fbuf, True)` (the `--delete` caller,
/// `delete.c:167`) goes directly to `do_rename_at` and reports RENAME.
/// upstream: `backup.c:200-219` - `do_link_at` then `do_rename_at`. A
/// pre-existing backup (`EEXIST`) is removed and the link retried
/// (`backup.c:247-256`).
///
/// Backup parents are created through upstream's `copy_valid_path`
/// (backup.c:157-184 `get_backup_name` -> :61-154 `copy_valid_path`): under
/// `--backup-dir` (`backup_dir` is `Some`) each freshly-created subdirectory
/// inherits its source directory's attributes and any non-directory obstruction
/// is cleared before it is recreated as a directory, via the shared
/// [`engine::create_backup_dir_parents`] helper. Without `--backup-dir`
/// (`backup_dir` is `None`) the backup lands alongside the destination whose
/// parent already exists, so upstream runs no `copy_valid_path` (backup.c:159)
/// and a plain `create_dir_all` suffices - byte-identical to the prior path.
#[cfg(any(unix, windows))]
fn place_existing_backup(
    existing: &Path,
    backup_path: &Path,
    prefer_rename: bool,
    dest_dir: &Path,
    backup_dir: Option<&Path>,
    metadata_opts: &MetadataOptions,
) -> io::Result<BackupPlacement> {
    if let Some(parent) = backup_path.parent() {
        match backup_dir {
            // upstream: backup.c:157-184 get_backup_name -> copy_valid_path -
            // with --backup-dir every new subdirectory inherits its source
            // dir's attrs and any non-directory obstruction is cleared before
            // it is recreated as a directory.
            Some(backup_dir) => {
                engine::create_backup_dir_parents(
                    dest_dir,
                    backup_dir,
                    parent,
                    metadata_opts,
                    |path| fs::create_dir_all(path),
                )?;
            }
            // upstream: backup.c:159 - without --backup-dir the parent already
            // exists next to the destination and no copy_valid_path runs.
            None if parent.exists() => {}
            None => fs::create_dir_all(parent)?,
        }
    }

    // upstream: backup.c:191 - the caller's `prefer_rename` suppresses the
    // hard-link tier entirely. Renaming is atomic, so the victim never exists
    // under two names the way a link+unlink pair would leave it.
    if prefer_rename {
        return rename_or_copy_existing(existing, backup_path);
    }

    match fast_io::hard_link(existing, backup_path) {
        Ok(()) => Ok(BackupPlacement::Hardlinked),
        // upstream: backup.c:247-256 - delete a stale backup and retry the link.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(backup_path);
            match fast_io::hard_link(existing, backup_path) {
                Ok(()) => Ok(BackupPlacement::Hardlinked),
                Err(_) => rename_or_copy_existing(existing, backup_path),
            }
        }
        // upstream: backup.c:210 - rename fallback when the item cannot be
        // hard-linked (cross-device, or a type/fs without CAN_HARDLINK_*).
        Err(_) => rename_or_copy_existing(existing, backup_path),
    }
}

/// Renames `existing` to `backup_path`, falling back to recreating the node on
/// a different filesystem when the rename fails cross-device (`EXDEV`).
///
/// upstream: `backup.c:226` `make_backup()` - once `link_or_rename()` cannot
/// move the item across the mount (a `--backup-dir` on another filesystem),
/// rsync makes a copy: `copy_file()` for regular files, or recreates the node
/// via `do_symlink_at`/`do_mknod_at` for symlinks and specials
/// (`backup.c:288-300`), then `keep_backup` unlinks the source.
#[cfg(any(unix, windows))]
fn rename_or_copy_existing(existing: &Path, backup_path: &Path) -> io::Result<BackupPlacement> {
    match fs::rename(existing, backup_path) {
        Ok(()) => Ok(BackupPlacement::Renamed),
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_existing_cross_device(existing, backup_path)
        }
        Err(e) => Err(e),
    }
}

/// Cross-device copy tier for a non-regular entry: recreates the symlink,
/// FIFO, socket, or device node at `backup_path`, then unlinks the original.
///
/// upstream: `backup.c:288-300` `make_backup()` copy tier - `do_mknod_at` for
/// devices/specials (SYMLINK/DEVICE traces) and `do_symlink_at` for symlinks,
/// used when neither hard-link nor rename can cross the filesystem boundary.
#[cfg(unix)]
fn copy_existing_cross_device(existing: &Path, backup_path: &Path) -> io::Result<BackupPlacement> {
    use std::os::unix::fs::FileTypeExt;

    let meta = fs::symlink_metadata(existing)?;
    let file_type = meta.file_type();
    let placement = if file_type.is_symlink() {
        // upstream: backup.c:296-300 - do_symlink_at recreates the link target.
        let target = fs::read_link(existing)?;
        std::os::unix::fs::symlink(&target, backup_path)?;
        BackupPlacement::CopiedSymlink
    } else if file_type.is_fifo() || file_type.is_socket() {
        // upstream: backup.c:288-291 - IS_SPECIAL -> do_mknod_at.
        metadata::create_fifo(backup_path, &meta).map_err(io::Error::other)?;
        BackupPlacement::CopiedNode
    } else if file_type.is_block_device() || file_type.is_char_device() {
        // upstream: backup.c:288-291 - IS_DEVICE -> do_mknod_at (needs root).
        metadata::create_device_node(backup_path, &meta).map_err(io::Error::other)?;
        BackupPlacement::CopiedNode
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cannot back up unsupported non-regular file across filesystems",
        ));
    };
    // upstream: keep_backup unlinks the source once the copy tier recreates it.
    fs::remove_file(existing)?;
    Ok(placement)
}

/// Emits the `--debug=BACKUP` mechanism trace and the `--info=BACKUP` notice
/// for a completed backup of `existing` to `backup_path`, both relative to
/// `dest_dir` so the wording matches upstream `testsuite/backup.test`.
#[cfg(any(unix, windows))]
fn report_backup(
    placement: &BackupPlacement,
    existing: &Path,
    backup_path: &Path,
    dest_dir: &Path,
) {
    let existing_display = existing.display().to_string();
    match placement {
        // upstream: backup.c:201-202 - DEBUG_GTE(BACKUP, 1) HLINK success.
        BackupPlacement::Hardlinked => engine::trace_make_backup_hlink(&existing_display),
        // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1) RENAME success.
        BackupPlacement::Renamed => engine::trace_make_backup_rename(&existing_display),
        // upstream: backup.c:299-300 - DEBUG_GTE(BACKUP, 1) SYMLINK success.
        BackupPlacement::CopiedSymlink => engine::trace_make_backup_symlink(&existing_display),
        // upstream: backup.c:290-291 - DEBUG_GTE(BACKUP, 1) DEVICE success.
        BackupPlacement::CopiedNode => engine::trace_make_backup_device(&existing_display),
    }
    // upstream: backup.c:352-353 - INFO_GTE(BACKUP, 1) "backed up %s to %s".
    let file_rel = existing.strip_prefix(dest_dir).unwrap_or(existing);
    let backup_rel = backup_path.strip_prefix(dest_dir).unwrap_or(backup_path);
    logging::info_log!(
        Backup,
        1,
        "backed up {} to {}",
        file_rel.display(),
        backup_rel.display()
    );
}

/// Reports whether `name` already ends with the backup `suffix`.
///
/// upstream: `delete.c:37-41` `is_backup_file()` - a non-empty suffix that
/// matches the trailing bytes of the name, with at least one byte preceding it
/// (`k = strlen(fn) - backup_suffix_len; k > 0 && strcmp(fn+k, suffix) == 0`).
#[cfg(any(unix, windows))]
pub(in crate::receiver::directory) fn is_backup_file(name: &OsStr, suffix: &str) -> bool {
    if suffix.is_empty() {
        return false;
    }
    let name = name.as_encoded_bytes();
    let suffix = suffix.as_bytes();
    name.len() > suffix.len() && name.ends_with(suffix)
}

/// Moves `existing` into its computed backup location, emits the trace, and
/// clears the original path so the caller need not unlink again.
///
/// Shared by the pre-replace and pre-delete backup callers, which differ only
/// in `prefer_rename`: `generator.c:2018` passes `False` (hard-link first)
/// while `delete.c:167` passes `True` (rename only). Only the hard-link
/// placement leaves the original behind (upstream's copy tier, `ok == 2`); the
/// rename and cross-device copy tiers already free the path.
///
/// upstream: `delete.c:167-170` - `make_backup(fbuf, True)` then, when the copy
/// tier left the original in place (`ok == 2`), `robust_unlink(fbuf)`.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn place_report_and_clear(
    existing: &Path,
    relative_path: &Path,
    dest_dir: &Path,
    backup_dir: Option<&Path>,
    suffix: &OsStr,
    sandbox: Option<&fast_io::DirSandbox>,
    prefer_rename: bool,
    metadata_opts: &MetadataOptions,
) -> io::Result<()> {
    let backup_path = engine::compute_backup_path(dest_dir, existing, None, backup_dir, suffix);
    let placement = place_existing_backup(
        existing,
        &backup_path,
        prefer_rename,
        dest_dir,
        backup_dir,
        metadata_opts,
    )?;
    report_backup(&placement, existing, &backup_path, dest_dir);
    if matches!(placement, BackupPlacement::Hardlinked) {
        // upstream: delete.c:169-170 - the hard-link tier is upstream's `ok == 2`
        // here: the original survives as a second link and must be unlinked.
        // SEC-1.g: route through the sandbox dirfd when the parent is the root.
        let _ = fast_io::unlink_via_sandbox_or_fallback(
            sandbox,
            dest_dir,
            relative_path,
            existing,
            fast_io::UnlinkFlags::File,
        );
    }
    Ok(())
}

/// Windows variant of [`place_report_and_clear`]: no dirfd sandbox, so the
/// leftover-original unlink is path-based.
#[cfg(windows)]
fn place_report_and_clear(
    existing: &Path,
    dest_dir: &Path,
    backup_dir: Option<&Path>,
    suffix: &OsStr,
    prefer_rename: bool,
    metadata_opts: &MetadataOptions,
) -> io::Result<()> {
    let backup_path = engine::compute_backup_path(dest_dir, existing, None, backup_dir, suffix);
    let placement = place_existing_backup(
        existing,
        &backup_path,
        prefer_rename,
        dest_dir,
        backup_dir,
        metadata_opts,
    )?;
    report_backup(&placement, existing, &backup_path, dest_dir);
    if matches!(placement, BackupPlacement::Hardlinked) {
        let _ = fs::remove_file(existing);
    }
    Ok(())
}

/// Backs up an extraneous destination file before the `--delete` pass unlinks
/// it, applying upstream's `is_backup_file` guard.
///
/// Returns `Ok(true)` when the victim was preserved into the backup area and
/// its original path is now clear (the caller must NOT unlink again); `Ok(false)`
/// when no backup applies (`--backup` off, an already-suffixed name with no
/// `--backup-dir`, or nothing at `existing`); `Err` when the backup mechanism
/// failed, which the caller treats as a failed deletion (upstream `DR_FAILURE`).
///
/// upstream: `delete.c:165-170` - `make_backups > 0 && !(flags & DEL_FOR_BACKUP)
/// && (backup_dir || !is_backup_file(fbuf))` guards `make_backup(fbuf, True)`.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(in crate::receiver::directory) fn backup_victim(
    backup: bool,
    backup_dir: Option<&Path>,
    suffix: &str,
    existing: &Path,
    relative_path: &Path,
    dest_dir: &Path,
    sandbox: Option<&fast_io::DirSandbox>,
    metadata_opts: &MetadataOptions,
) -> io::Result<bool> {
    if !backup {
        return Ok(false);
    }
    // upstream: delete.c:165 - DEL_FOR_BACKUP is never set on this delete path,
    // so the guard reduces to `backup_dir || !is_backup_file(name)`: a file
    // already ending in the suffix is unlinked directly unless a --backup-dir
    // sends it to the separate directory (no `<name><suffix><suffix>`).
    if backup_dir.is_none()
        && existing
            .file_name()
            .is_some_and(|name| is_backup_file(name, suffix))
    {
        return Ok(false);
    }
    // upstream: backup.c:236 - a vanished source (x_lstat != 0) needs no backup.
    if fs::symlink_metadata(existing).is_err() {
        return Ok(false);
    }
    // upstream: delete.c:167 - `make_backup(fbuf, True)`. The delete pass is the
    // `prefer_rename` caller: the victim is renamed into the backup area, so the
    // RENAME trace is emitted and no second link ever exists.
    place_report_and_clear(
        existing,
        relative_path,
        dest_dir,
        backup_dir,
        OsStr::new(suffix),
        sandbox,
        true,
        metadata_opts,
    )?;
    Ok(true)
}

/// Windows variant of [`backup_victim`]: no dirfd sandbox is threaded through.
#[cfg(windows)]
pub(in crate::receiver::directory) fn backup_victim(
    backup: bool,
    backup_dir: Option<&Path>,
    suffix: &str,
    existing: &Path,
    dest_dir: &Path,
    metadata_opts: &MetadataOptions,
) -> io::Result<bool> {
    if !backup {
        return Ok(false);
    }
    if backup_dir.is_none()
        && existing
            .file_name()
            .is_some_and(|name| is_backup_file(name, suffix))
    {
        return Ok(false);
    }
    if fs::symlink_metadata(existing).is_err() {
        return Ok(false);
    }
    // upstream: delete.c:167 - `make_backup(fbuf, True)`, the rename-only caller.
    place_report_and_clear(
        existing,
        dest_dir,
        backup_dir,
        OsStr::new(suffix),
        true,
        metadata_opts,
    )?;
    Ok(true)
}

/// Backs up (when configured) then unlinks a single extraneous file victim.
///
/// The shared file-removal step for every `--delete` site: the immediate
/// parallel scan, the deferred `--delete-delay` executor, and the capped
/// serial executor. When the backup took ownership of the victim the direct
/// unlink is skipped.
///
/// upstream: `delete.c:165-174` - back up under the guard, otherwise unlink.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(in crate::receiver::directory) fn remove_file_victim(
    backup: bool,
    backup_dir: Option<&Path>,
    suffix: &str,
    existing: &Path,
    relative_path: &Path,
    dest_dir: &Path,
    sandbox: Option<&fast_io::DirSandbox>,
    metadata_opts: &MetadataOptions,
) -> io::Result<()> {
    if backup_victim(
        backup,
        backup_dir,
        suffix,
        existing,
        relative_path,
        dest_dir,
        sandbox,
        metadata_opts,
    )? {
        return Ok(());
    }
    fast_io::unlink_via_sandbox_or_fallback(
        sandbox,
        dest_dir,
        relative_path,
        existing,
        fast_io::UnlinkFlags::File,
    )
}

/// Non-Unix variant of [`remove_file_victim`]: path-based removal, with the
/// backup step wired only on Windows where the mechanism is supported.
#[cfg(not(unix))]
pub(in crate::receiver::directory) fn remove_file_victim(
    backup: bool,
    backup_dir: Option<&std::path::Path>,
    suffix: &str,
    existing: &std::path::Path,
    dest_dir: &std::path::Path,
    metadata_opts: &MetadataOptions,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        if backup_victim(backup, backup_dir, suffix, existing, dest_dir, metadata_opts)? {
            return Ok(());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (backup, backup_dir, suffix, dest_dir, metadata_opts);
    }
    std::fs::remove_file(existing)
}

impl ReceiverContext {
    /// Backs up an existing destination entry before it is replaced by a fresh
    /// symlink, FIFO, socket, or device node.
    ///
    /// Returns `Ok(true)` when a backup was made (the original was hard-linked
    /// or renamed into the backup area and its original path is now clear for
    /// the replacement). Returns `Ok(false)` when no backup applies - either
    /// `--backup` is off or nothing exists at `existing` to preserve - in which
    /// case the caller removes any obstacle itself. `Err` signals that the
    /// backup mechanism failed; the caller skips creating the replacement,
    /// mirroring upstream `atomic_create` returning 0 when `make_backup` fails.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2018-2020` - `if (make_backups > 0 ...) if (!make_backup(fname, ...)) return 0;`
    #[cfg(unix)]
    pub(in crate::receiver) fn backup_existing_before_replace(
        &self,
        existing: &Path,
        relative_path: &Path,
        dest_dir: &Path,
        sandbox: Option<&fast_io::DirSandbox>,
    ) -> io::Result<bool> {
        if !self.config.flags.backup {
            return Ok(false);
        }
        // upstream: backup.c:236 - x_lstat returns 3 (nothing to keep) when the
        // path is absent; a fresh create needs no backup.
        if fs::symlink_metadata(existing).is_err() {
            return Ok(false);
        }

        // upstream: generator.c:2018 - `make_backup(fname, False)`, so the
        // hard-link tier runs first for a symlink/special replacement.
        let metadata_opts = self.build_metadata_options();
        place_report_and_clear(
            existing,
            relative_path,
            dest_dir,
            self.config.backup_dir.as_deref().map(Path::new),
            OsStr::new(self.config.effective_backup_suffix()),
            sandbox,
            false,
            &metadata_opts,
        )?;
        Ok(true)
    }

    /// Windows variant: no dirfd sandbox, so the post-hard-link removal uses a
    /// path-based `remove_file`, matching the Windows symlink-create path.
    #[cfg(windows)]
    pub(in crate::receiver) fn backup_existing_before_replace(
        &self,
        existing: &Path,
        _relative_path: &Path,
        dest_dir: &Path,
    ) -> io::Result<bool> {
        if !self.config.flags.backup {
            return Ok(false);
        }
        if fs::symlink_metadata(existing).is_err() {
            return Ok(false);
        }

        // upstream: generator.c:2018 - `make_backup(fname, False)`.
        let metadata_opts = self.build_metadata_options();
        place_report_and_clear(
            existing,
            dest_dir,
            self.config.backup_dir.as_deref().map(Path::new),
            OsStr::new(self.config.effective_backup_suffix()),
            false,
            &metadata_opts,
        )?;
        Ok(true)
    }
}

#[cfg(all(test, unix))]
mod tests {
    //! Mechanism-level pins for the non-regular backup: an existing symlink or
    //! FIFO is preserved in the backup area, and the `--debug=BACKUP` trace is
    //! emitted (HLINK where hard-linking the type is supported) and suppressed
    //! below level 1.

    use super::{
        BackupPlacement, backup_victim, copy_existing_cross_device, place_existing_backup,
        report_backup,
    };
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
    use metadata::MetadataOptions;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::path::Path;

    fn backup_debug_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Backup,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// An existing symlink must survive in the backup location with its target
    /// intact. On Linux hard-linking a symlink is supported, so the mechanism
    /// takes the HLINK branch and emits the upstream `make_backup: HLINK` line.
    #[test]
    fn existing_symlink_preserved_in_backup_with_target() {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.backup = 1;
        init(cfg);
        let _ = drain_events();

        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        let backup = dir.path().join("link~");
        std::os::unix::fs::symlink("original/target", &link).unwrap();

        let placement = place_existing_backup(
            &link,
            &backup,
            false,
            dir.path(),
            None,
            &::metadata::MetadataOptions::default(),
        )
        .unwrap();

        // On Linux `link(2)` does not dereference a symlink source, so the
        // backup is taken via hard-link and the HLINK trace is guaranteed.
        // upstream: configure.ac:1067 defines CAN_HARDLINK_SYMLINK on Linux.
        #[cfg(target_os = "linux")]
        assert!(
            matches!(placement, BackupPlacement::Hardlinked),
            "Linux must hard-link the symlink backup (HLINK branch)"
        );

        // Backup carries the original target, byte-for-byte.
        assert_eq!(
            fs::read_link(&backup).unwrap(),
            std::path::Path::new("original/target"),
            "backup must preserve the original symlink target"
        );

        // place_existing_backup itself must not emit any trace.
        assert!(
            backup_debug_messages().is_empty(),
            "place_existing_backup must not trace"
        );

        // upstream: backup.c:201-202 / :216-217 - report_backup emits exactly
        // one mechanism trace, HLINK when the symlink was hard-linked.
        report_backup(&placement, &link, &backup, dir.path());
        let expected = match placement {
            BackupPlacement::Hardlinked => {
                format!("make_backup: HLINK {} successful.", link.display())
            }
            BackupPlacement::Renamed => {
                format!("make_backup: RENAME {} successful.", link.display())
            }
            BackupPlacement::CopiedSymlink => {
                format!("make_backup: SYMLINK {} successful.", link.display())
            }
            BackupPlacement::CopiedNode => {
                format!("make_backup: DEVICE {} successful.", link.display())
            }
        };
        assert!(
            backup_debug_messages().contains(&expected),
            "missing mechanism trace {expected:?}"
        );
    }

    /// An existing FIFO must survive in the backup location as a FIFO.
    #[test]
    fn existing_fifo_preserved_in_backup() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let backup = dir.path().join("pipe~");
        // mkfifo via metadata's safe wrapper needs no privilege.
        metadata::create_fifo_node_from_parts(&fifo, 0o644, false, false).unwrap();

        place_existing_backup(
            &fifo,
            &backup,
            false,
            dir.path(),
            None,
            &::metadata::MetadataOptions::default(),
        )
        .unwrap();

        assert!(
            fs::symlink_metadata(&backup).unwrap().file_type().is_fifo(),
            "backup of a FIFO must itself be a FIFO"
        );
    }

    /// The `--backup --delete` caller passes `prefer_rename = true`, so the
    /// hard-link tier must be skipped entirely and the victim renamed into the
    /// backup area: the mechanism is RENAME, the trace says RENAME, and the
    /// original path is already free (no link+unlink window).
    ///
    /// Inode identity does not discriminate here - a same-filesystem rename and
    /// a hard link both leave the pre-image inode at the backup name - so the
    /// assertion is on the emitted mechanism, which is the observable upstream
    /// difference (`make_backup: RENAME` vs `make_backup: HLINK`).
    ///
    /// upstream: delete.c:167 `make_backup(fbuf, True)` -> backup.c:191
    /// `if (!prefer_rename)` skips `do_link_at`, so `do_rename_at` reports
    /// "make_backup: RENAME %s successful." (backup.c:216-217).
    #[test]
    fn prefer_rename_skips_hard_link_tier() {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.backup = 1;
        init(cfg);
        let _ = drain_events();

        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("extraneous");
        let backup = dir.path().join("extraneous~");
        fs::write(&victim, b"victim payload").unwrap();

        let placement = place_existing_backup(
            &victim,
            &backup,
            true,
            dir.path(),
            None,
            &::metadata::MetadataOptions::default(),
        )
        .unwrap();

        assert!(
            matches!(placement, BackupPlacement::Renamed),
            "prefer_rename must take the RENAME branch, never HLINK"
        );
        assert!(
            fs::symlink_metadata(&victim).is_err(),
            "the rename must free the victim path outright"
        );
        assert_eq!(fs::read(&backup).unwrap(), b"victim payload");

        report_backup(&placement, &victim, &backup, dir.path());
        assert!(
            backup_debug_messages().contains(&format!(
                "make_backup: RENAME {} successful.",
                victim.display()
            )),
            "the delete-pass backup must trace RENAME, not HLINK"
        );
    }

    /// Control for [`prefer_rename_skips_hard_link_tier`]: the same fixture with
    /// `prefer_rename = false` (the `generator.c:2018` pre-replace caller) takes
    /// the hard-link tier instead, proving the flag - not the file type or the
    /// filesystem - is what selects the mechanism.
    ///
    /// upstream: backup.c:201-202 - `do_link_at` success emits
    /// "make_backup: HLINK %s successful." and returns 2, leaving the original
    /// in place for the caller to replace.
    #[test]
    fn hard_link_tier_traces_hlink_for_regular_file() {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.backup = 1;
        init(cfg);
        let _ = drain_events();

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular");
        let backup = dir.path().join("regular~");
        fs::write(&file, b"pre-image").unwrap();

        let placement = place_existing_backup(
            &file,
            &backup,
            false,
            dir.path(),
            None,
            &::metadata::MetadataOptions::default(),
        )
        .unwrap();

        assert!(
            matches!(placement, BackupPlacement::Hardlinked),
            "a same-filesystem regular file must hard-link (HLINK branch)"
        );
        assert!(
            fs::symlink_metadata(&file).is_ok(),
            "the hard-link tier must leave the original in place (upstream ok == 2)"
        );

        report_backup(&placement, &file, &backup, dir.path());
        assert!(
            backup_debug_messages().contains(&format!(
                "make_backup: HLINK {} successful.",
                file.display()
            )),
            "the pre-replace backup must trace HLINK"
        );
    }

    /// A cross-device backup of an existing symlink must succeed via the copy
    /// tier: the link is recreated at the backup path with its target intact
    /// and the original is unlinked, mirroring upstream's `do_symlink_at`
    /// fallback when neither hard-link nor rename can cross the filesystem.
    ///
    /// upstream: backup.c:296-300 make_backup() SYMLINK copy tier.
    #[test]
    fn cross_device_symlink_recreated_at_backup() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        let backup = dir.path().join("link~");
        std::os::unix::fs::symlink("original/target", &link).unwrap();

        let placement = copy_existing_cross_device(&link, &backup).unwrap();
        assert!(matches!(placement, BackupPlacement::CopiedSymlink));

        assert_eq!(
            fs::read_link(&backup).unwrap(),
            std::path::Path::new("original/target"),
            "recreated backup must preserve the symlink target"
        );
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "original symlink must be unlinked after the copy tier"
        );
    }

    /// A cross-device backup of an existing FIFO must succeed via the copy
    /// tier: the node is recreated at the backup path as a FIFO and the
    /// original is unlinked (upstream `do_mknod_at` IS_SPECIAL branch).
    ///
    /// upstream: backup.c:288-291 make_backup() DEVICE copy tier.
    #[test]
    fn cross_device_fifo_recreated_at_backup() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let backup = dir.path().join("pipe~");
        metadata::create_fifo_node_from_parts(&fifo, 0o644, false, false).unwrap();

        let placement = copy_existing_cross_device(&fifo, &backup).unwrap();
        assert!(matches!(placement, BackupPlacement::CopiedNode));

        assert!(
            fs::symlink_metadata(&backup).unwrap().file_type().is_fifo(),
            "recreated backup of a FIFO must itself be a FIFO"
        );
        assert!(
            fs::symlink_metadata(&fifo).is_err(),
            "original FIFO must be unlinked after the copy tier"
        );
    }

    /// Level 0 must suppress the mechanism trace entirely.
    #[test]
    fn debug_backup_level_zero_suppresses_trace() {
        let cfg = VerbosityConfig::default();
        init(cfg);
        let _ = drain_events();

        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l");
        let backup = dir.path().join("l~");
        std::os::unix::fs::symlink("t", &link).unwrap();

        let placement = place_existing_backup(
            &link,
            &backup,
            false,
            dir.path(),
            None,
            &::metadata::MetadataOptions::default(),
        )
        .unwrap();
        report_backup(&placement, &link, &backup, dir.path());

        assert!(
            backup_debug_messages().is_empty(),
            "debug=BACKUP level 0 must suppress the mechanism trace"
        );
    }

    /// Under `--backup-dir`, a non-regular (symlink) victim whose backup
    /// subdirectory does not yet exist in the backup tree routes through
    /// upstream's `copy_valid_path` (backup.c:157-184 `get_backup_name` ->
    /// :61-154 `copy_valid_path`), so the freshly-created subdirectory inherits
    /// the corresponding destination directory's mode rather than the umask
    /// default. Before the fix, `place_existing_backup` created the parent with
    /// a bare `create_dir_all`, leaving it at umask perms; this test pins the
    /// attribute-inheritance contract.
    ///
    /// upstream: backup.c:115-138 - after `do_mkdir_at` each new subdir gets
    /// `x_stat(rel) + make_file + set_file_attrs`, copying the source dir mode.
    #[test]
    fn backup_dir_new_subdir_inherits_dest_dir_mode() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let sub = dest.join("sub");
        fs::create_dir_all(&sub).unwrap();
        // Distinctive, non-umask-default mode the backup subdir must inherit.
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();

        // Backup root does not exist yet, so its "sub" element is created fresh
        // by copy_valid_path and must inherit dest/sub's 0700.
        let backup_root = dir.path().join("bak");
        let victim = sub.join("link");
        std::os::unix::fs::symlink("target", &victim).unwrap();

        // upstream: backup.c:137 set_file_attrs honours the transfer's
        // preserve-* policy; here only permissions are preserved.
        let opts = MetadataOptions::new().preserve_permissions(true);
        // With --backup-dir the suffix is empty (backup.c:159 vs :179).
        let backup_path = engine::compute_backup_path(
            &dest,
            &victim,
            None,
            Some(backup_root.as_path()),
            OsStr::new(""),
        );

        let placement = place_existing_backup(
            &victim,
            &backup_path,
            false,
            &dest,
            Some(backup_root.as_path()),
            &opts,
        )
        .unwrap();

        // The symlink is preserved into the backup area (Linux hard-links it).
        #[cfg(target_os = "linux")]
        assert!(matches!(placement, BackupPlacement::Hardlinked));
        let _ = &placement;
        assert_eq!(
            fs::read_link(&backup_path).unwrap(),
            Path::new("target"),
            "backup must preserve the symlink target"
        );

        let created_sub = backup_root.join("sub");
        let mode = fs::symlink_metadata(&created_sub)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o700,
            "new --backup-dir subdirectory must inherit the dest dir mode, not umask default"
        );
    }

    /// Under `--backup-dir`, a non-directory obstruction sitting where a backup
    /// subdirectory must go is cleared and recreated as a directory, mirroring
    /// upstream `validate_backup_dir` (backup.c:48-53), where a non-directory
    /// triggers `delete_item(...DEL_FOR_BACKUP|DEL_RECURSE)`. Before the fix the
    /// bare `create_dir_all` surfaced `NotADirectory` and aborted the backup.
    ///
    /// upstream: backup.c:48-53 / :101-113 - the obstruction is removed so the
    /// element can be recreated via `do_mkdir_at`.
    #[test]
    fn backup_dir_nondir_obstruction_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let sub = dest.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let backup_root = dir.path().join("bak");
        fs::create_dir_all(&backup_root).unwrap();
        // Plant a regular file where the "sub" backup subdirectory must live.
        fs::write(backup_root.join("sub"), b"obstruction").unwrap();

        let victim = sub.join("link");
        std::os::unix::fs::symlink("target", &victim).unwrap();

        let opts = MetadataOptions::new().preserve_permissions(true);
        let backup_path = engine::compute_backup_path(
            &dest,
            &victim,
            None,
            Some(backup_root.as_path()),
            OsStr::new(""),
        );

        // Must not fail with NotADirectory: the obstruction is cleared first.
        place_existing_backup(
            &victim,
            &backup_path,
            false,
            &dest,
            Some(backup_root.as_path()),
            &opts,
        )
        .unwrap();

        assert!(
            fs::symlink_metadata(backup_root.join("sub"))
                .unwrap()
                .is_dir(),
            "the non-directory obstruction must be replaced by a directory"
        );
        assert_eq!(
            fs::read_link(&backup_path).unwrap(),
            Path::new("target"),
            "backup must land inside the recreated directory"
        );
    }

    /// The `--delete` pre-delete backup path (`backup_victim` ->
    /// `place_report_and_clear`, upstream delete.c:167 `make_backup(fbuf, True)`)
    /// routes a non-regular victim's `--backup-dir` parents through the same
    /// `copy_valid_path` attribute inheritance: a freshly-created backup
    /// subdirectory inherits the destination directory's mode, and the victim is
    /// renamed into it (prefer_rename), freeing the original path.
    ///
    /// upstream: delete.c:167 -> backup.c:157-184 get_backup_name ->
    /// copy_valid_path (backup.c:115-138 set_file_attrs).
    #[test]
    fn delete_backup_dir_new_subdir_inherits_dest_dir_mode() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let sub = dest.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o750)).unwrap();

        let backup_root = dir.path().join("bak");
        let victim = sub.join("gone");
        std::os::unix::fs::symlink("was-here", &victim).unwrap();

        let opts = MetadataOptions::new().preserve_permissions(true);
        let backed_up = backup_victim(
            true,
            Some(backup_root.as_path()),
            "", // --backup-dir uses an empty suffix
            &victim,
            Path::new("sub/gone"),
            &dest,
            None,
            &opts,
        )
        .unwrap();
        assert!(backed_up, "the victim must be preserved into the backup area");

        // prefer_rename frees the original path outright (no link+unlink window).
        assert!(
            fs::symlink_metadata(&victim).is_err(),
            "the delete-pass rename must free the victim path"
        );

        let backup_path = backup_root.join("sub").join("gone");
        assert_eq!(
            fs::read_link(&backup_path).unwrap(),
            Path::new("was-here"),
            "the renamed backup must preserve the symlink target"
        );
        let mode = fs::symlink_metadata(backup_root.join("sub"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o750,
            "the --delete --backup-dir subdir must inherit the dest dir mode"
        );
    }
}
