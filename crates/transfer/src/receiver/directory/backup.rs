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
use std::fs;
#[cfg(any(unix, windows))]
use std::io;
#[cfg(any(unix, windows))]
use std::path::Path;

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
}

/// Places `existing` into `backup_path`, mirroring upstream `link_or_rename`
/// with `prefer_rename = 0`: hard-link first, rename on fallback.
///
/// upstream: `backup.c:200-219` - `do_link_at` then `do_rename_at`. A
/// pre-existing backup (`EEXIST`) is removed and the link retried
/// (`backup.c:247-256`).
#[cfg(any(unix, windows))]
fn place_existing_backup(existing: &Path, backup_path: &Path) -> io::Result<BackupPlacement> {
    if let Some(parent) = backup_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    match fast_io::hard_link(existing, backup_path) {
        Ok(()) => Ok(BackupPlacement::Hardlinked),
        // upstream: backup.c:247-256 - delete a stale backup and retry the link.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(backup_path);
            match fast_io::hard_link(existing, backup_path) {
                Ok(()) => Ok(BackupPlacement::Hardlinked),
                Err(_) => {
                    fs::rename(existing, backup_path)?;
                    Ok(BackupPlacement::Renamed)
                }
            }
        }
        // upstream: backup.c:210 - rename fallback when the item cannot be
        // hard-linked (cross-device, or a type/fs without CAN_HARDLINK_*).
        Err(_) => {
            fs::rename(existing, backup_path)?;
            Ok(BackupPlacement::Renamed)
        }
    }
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

        let backup_path = engine::compute_backup_path(
            dest_dir,
            existing,
            None,
            self.config.backup_dir.as_deref().map(Path::new),
            std::ffi::OsStr::new(self.config.effective_backup_suffix()),
        );

        let placement = place_existing_backup(existing, &backup_path)?;
        report_backup(&placement, existing, &backup_path, dest_dir);

        if matches!(placement, BackupPlacement::Hardlinked) {
            // The original still exists as a second link; remove it so the new
            // node can take its place. upstream: atomic_create's rename over
            // `fname` does this implicitly.
            //
            // SEC-1.g: route the removal through the sandbox dirfd when the
            // destination parent is the sandbox root so a TOCTOU swap on the
            // original path cannot redirect the unlink.
            let _ = fast_io::unlink_via_sandbox_or_fallback(
                sandbox,
                dest_dir,
                relative_path,
                existing,
                fast_io::UnlinkFlags::File,
            );
        }
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

        let backup_path = engine::compute_backup_path(
            dest_dir,
            existing,
            None,
            self.config.backup_dir.as_deref().map(Path::new),
            std::ffi::OsStr::new(self.config.effective_backup_suffix()),
        );

        let placement = place_existing_backup(existing, &backup_path)?;
        report_backup(&placement, existing, &backup_path, dest_dir);

        if matches!(placement, BackupPlacement::Hardlinked) {
            let _ = fs::remove_file(existing);
        }
        Ok(true)
    }
}

#[cfg(all(test, unix))]
mod tests {
    //! Mechanism-level pins for the non-regular backup: an existing symlink or
    //! FIFO is preserved in the backup area, and the `--debug=BACKUP` trace is
    //! emitted (HLINK where hard-linking the type is supported) and suppressed
    //! below level 1.

    use super::{BackupPlacement, place_existing_backup, report_backup};
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
    use std::fs;
    use std::os::unix::fs::FileTypeExt;

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

        let placement = place_existing_backup(&link, &backup).unwrap();

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

        place_existing_backup(&fifo, &backup).unwrap();

        assert!(
            fs::symlink_metadata(&backup).unwrap().file_type().is_fifo(),
            "backup of a FIFO must itself be a FIFO"
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

        let placement = place_existing_backup(&link, &backup).unwrap();
        report_backup(&placement, &link, &backup, dir.path());

        assert!(
            backup_debug_messages().is_empty(),
            "debug=BACKUP level 0 must suppress the mechanism trace"
        );
    }
}
