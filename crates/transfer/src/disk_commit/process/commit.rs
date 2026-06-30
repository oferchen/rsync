//! Commit path for the disk commit thread: backup, atomic rename, inplace
//! truncation, cross-device fallback, and partial-file retention.
//!
//! Mirrors upstream `receiver.c` finalization and `cleanup.c` partial
//! handling. Rename uses io_uring `IORING_OP_RENAMEAT` when available, falling
//! back to `std::fs::rename` with a copy+remove EXDEV path
//! (`util1.c:robust_rename()`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use engine::{CleanupManager, compute_backup_path, trace_make_backup_rename};

use crate::pipeline::messages::{BackupNotice, BeginMessage};
use crate::temp_guard::TempFileGuard;

use super::super::config::{BackupConfig, DiskCommitConfig, PartialMode};

/// Commit result indicating whether a cross-device copy occurred and
/// whether the file was staged to the partial dir for delayed updates.
pub(super) struct CommitOutcome {
    /// True when a cross-device copy was needed (EXDEV fallback).
    pub(super) was_copy: bool,
    /// When `--delay-updates` staged the file to `.~tmp~`, holds the
    /// staging path. `None` for immediate commits and inplace writes.
    pub(super) delayed_path: Option<PathBuf>,
    /// Destination-relative paths recorded when `--backup` renamed an
    /// existing file. Propagated to the main thread via [`CommitResult`]
    /// so the upstream `INFO_GTE(BACKUP, 1)` notice can be emitted by the
    /// thread whose `VerbosityConfig` carries the user's `--info=backup`.
    pub(super) backup_notice: Option<BackupNotice>,
}

/// Performs backup, atomic rename, and inplace truncation after writing.
///
/// When `delay_updates` is true and the file uses temp+rename, stages the
/// file to `.~tmp~/<filename>` in the same parent directory instead of
/// renaming to the final destination. The caller reports the staging path
/// back so the receiver can perform a bulk rename sweep at phase 2.
///
/// When io_uring is available (Linux 5.11+ with `IORING_OP_RENAMEAT`), the
/// temp-file rename is submitted as an io_uring SQE instead of a synchronous
/// `rename(2)` syscall. Falls back to `std::fs::rename` on all other
/// platforms or when the kernel lacks the opcode.
///
/// # Upstream Reference
///
/// - `receiver.c:906-929`: delay_updates stages to partial dir
/// - `receiver.c:422-450`: `handle_delayed_updates()` bulk rename
pub(super) fn commit_file(
    begin: &BeginMessage,
    config: &DiskCommitConfig,
    cleanup_guard: &mut TempFileGuard,
    needs_rename: bool,
    bytes_written: u64,
) -> io::Result<CommitOutcome> {
    // upstream: backup.c:make_backup() - rename existing file before overwrite
    // With delay_updates, backup happens during the sweep, not here.
    let backup_notice = if !config.delay_updates {
        if let Some(ref backup_config) = config.backup {
            make_backup(&begin.file_path, backup_config)?
        } else {
            None
        }
    } else {
        None
    };

    if needs_rename && config.delay_updates {
        // upstream: receiver.c:916-929 - stage to partial dir (.~tmp~)
        let staging_path = partial_dir_path(&begin.file_path);
        if let Some(parent) = staging_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let result = rename_with_io_uring_fallback(cleanup_guard.path(), &staging_path)?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        cleanup_guard.keep();
        return Ok(CommitOutcome {
            was_copy: result,
            delayed_path: Some(staging_path),
            backup_notice,
        });
    }

    let was_copy = if needs_rename {
        let result = rename_with_io_uring_fallback(cleanup_guard.path(), &begin.file_path)?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        result
    } else if begin.is_inplace {
        // upstream: receiver.c:340 - set_file_length(fd, F_LENGTH(file))
        // In append mode, bytes_written only counts newly received data -
        // the full file size includes the existing content we seeked past.
        let final_size = if begin.append_offset > 0 {
            begin.target_size
        } else {
            bytes_written
        };
        let file = fs::OpenOptions::new().write(true).open(&begin.file_path)?;
        file.set_len(final_size)?;
        false
    } else {
        false
    };
    cleanup_guard.keep();
    Ok(CommitOutcome {
        was_copy,
        delayed_path: None,
        backup_notice,
    })
}

/// Upstream partial dir name for `--delay-updates` staging.
///
/// upstream: options.c - `static char tmp_partialdir[] = ".~tmp~";`
const DELAY_UPDATES_PARTIAL_DIR: &str = ".~tmp~";

/// Computes the `.~tmp~/<filename>` staging path for a destination file.
///
/// upstream: receiver.c:430 - `partial_dir_fname(fname)` returns
/// `<parent>/.~tmp~/<basename>`.
pub(super) fn partial_dir_path(file_path: &Path) -> PathBuf {
    let parent = file_path.parent().unwrap_or(Path::new("."));
    let basename = file_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("unknown"));
    parent.join(DELAY_UPDATES_PARTIAL_DIR).join(basename)
}

/// Retains a partial temp file instead of deleting it on interrupt.
///
/// Depending on the `PartialMode`:
/// - `None`: does nothing (the guard's Drop will delete the temp file).
/// - `Partial`: renames the temp file to the final destination path, so
///   the incomplete file is available for resume on the next run.
/// - `PartialDir(dir)`: moves the temp file into the partial directory,
///   using the destination filename as the target name.
///
/// Errors are logged and silently ignored - partial retention is best-effort.
/// On failure, the guard's Drop will clean up the temp file.
///
/// # Upstream Reference
///
/// - `cleanup.c:105-115` - `handle_partial_dir()` moves temp to partial-dir
/// - `cleanup.c:130-135` - `keep_partial && got_literal` guard
/// - `receiver.c:340-345` - `do_rename(partialptr, fname)` for `--partial`
pub(super) fn retain_partial_file(
    partial_mode: &PartialMode,
    cleanup_guard: &mut TempFileGuard,
    dest_path: &Path,
) {
    match partial_mode {
        PartialMode::None => {}
        PartialMode::Partial => {
            // upstream: cleanup.c:130-135 - rename temp file directly to
            // the destination. The incomplete content replaces any existing
            // file at the destination path.
            let temp_path = cleanup_guard.path().to_path_buf();
            match rename_with_io_uring_fallback(cleanup_guard.path(), dest_path) {
                Ok(_) => {
                    // upstream: cleanup.c:174-178 - stamp modtime=0 on
                    // retained partial files so --update does not skip them
                    // as "up to date" on the next run. Only for plain
                    // --partial, not --partial-dir (upstream uses
                    // handle_partial_dir() for --partial-dir which does not
                    // zero the mtime).
                    //
                    // Use from_unix_time(0, 0) rather than FileTime::zero()
                    // because on Windows, zero() maps to the Windows epoch
                    // (1601-01-01) which becomes an all-zero FILETIME -
                    // SetFileTime treats that as "do not change", silently
                    // skipping the stamp. from_unix_time(0, 0) maps to
                    // 1970-01-01 which is a non-zero FILETIME that Windows
                    // will actually apply.
                    let epoch = filetime::FileTime::from_unix_time(0, 0);
                    if let Err(e) = filetime::set_file_mtime(dest_path, epoch) {
                        logging::debug_log!(
                            Io,
                            1,
                            "failed to set mtime=0 on partial file {}: {}",
                            dest_path.display(),
                            e
                        );
                    }
                    logging::debug_log!(Io, 1, "retained partial file: {}", dest_path.display());
                    CleanupManager::global().unregister_temp_file(&temp_path);
                    cleanup_guard.keep();
                }
                Err(e) => {
                    logging::debug_log!(
                        Io,
                        1,
                        "failed to retain partial file {}: {}",
                        dest_path.display(),
                        e
                    );
                }
            }
        }
        PartialMode::PartialDir(dir) => {
            // upstream: cleanup.c:105-115 - move temp file into partial-dir
            let temp_path = cleanup_guard.path().to_path_buf();
            match cleanup_guard.rename_to_partial_dir(dest_path, dir) {
                Ok(partial_path) => {
                    CleanupManager::global().unregister_temp_file(&temp_path);
                    logging::debug_log!(
                        Io,
                        1,
                        "retained partial file in partial-dir: {}",
                        partial_path.display()
                    );
                }
                Err(e) => {
                    logging::debug_log!(
                        Io,
                        1,
                        "failed to retain partial file in {}: {}",
                        dir.display(),
                        e
                    );
                }
            }
        }
    }
}

/// Renames a temp file to its final destination, trying io_uring first.
///
/// Returns `Ok(false)` when the rename succeeded in-place, `Ok(true)` when
/// a cross-device copy+remove fallback was used (EXDEV). Callers use the
/// return value to decide whether metadata must be re-applied to the
/// destination.
///
/// On Linux 5.11+ with io_uring RENAMEAT2 support, submits the rename as an
/// `IORING_OP_RENAMEAT` SQE. Falls back to `std::fs::rename` when io_uring
/// is unavailable (non-Linux, old kernel, or feature not compiled in).
///
/// Cross-device fallback mirrors upstream `util1.c:robust_rename()` which
/// uses `copy_file()` + `do_unlink()` when `rename()` returns EXDEV. This
/// happens when `--temp-dir` points to a different filesystem than the
/// destination.
pub(super) fn rename_with_io_uring_fallback(old_path: &Path, new_path: &Path) -> io::Result<bool> {
    if let Some(result) = fast_io::try_rename_via_io_uring(old_path, new_path) {
        return result.map(|()| false);
    }
    match fs::rename(old_path, new_path) {
        Ok(()) => Ok(false),
        Err(e) if is_cross_device(&e) => {
            // upstream: util1.c:robust_rename() - copy_file + do_unlink
            fs::copy(old_path, new_path)?;
            fs::remove_file(old_path)?;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

/// Returns `true` when an I/O error represents a cross-device link (EXDEV).
///
/// On Unix, `raw_os_error() == libc::EXDEV` (errno 18). On Windows,
/// `ERROR_NOT_SAME_DEVICE` (error 17) is the equivalent.
pub(super) fn is_cross_device(e: &io::Error) -> bool {
    match e.raw_os_error() {
        #[cfg(unix)]
        Some(code) => code == libc::EXDEV,
        #[cfg(windows)]
        Some(code) => code == 17, // ERROR_NOT_SAME_DEVICE
        #[cfg(not(any(unix, windows)))]
        Some(_) => false,
        None => false,
    }
}

/// Creates a backup of the destination file before overwriting.
///
/// Mirrors upstream `backup.c:make_backup()` which renames the existing file
/// to the backup path. Parent directories are created if needed when using
/// `--backup-dir`. On success, emits the upstream `--debug=BACKUP` RENAME
/// notice (`backup.c:216-217`) and returns a [`BackupNotice`] carrying the
/// destination-relative paths so the main thread can emit upstream's
/// `INFO_GTE(BACKUP, 1)` line (`backup.c:352`). The disk thread cannot emit
/// the info line directly because its thread-local [`logging::VerbosityConfig`]
/// is never seeded with the user's `--info=backup` selection.
pub(super) fn make_backup(
    file_path: &Path,
    config: &BackupConfig,
) -> io::Result<Option<BackupNotice>> {
    if !file_path.exists() {
        return Ok(None);
    }

    let backup_path = compute_backup_path(
        &config.dest_dir,
        file_path,
        None,
        config.backup_dir.as_deref(),
        &config.suffix,
    );

    if let Some(parent) = backup_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    fs::rename(file_path, &backup_path)?;
    // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1) on the RENAME success
    // branch of link_or_rename. disk_commit always uses rename here.
    trace_make_backup_rename(&file_path.display().to_string());
    // upstream: backup.c:352 - INFO_GTE(BACKUP, 1) fires on success label for
    // every successful backup. Paths are displayed relative to the destination
    // root to match upstream test assertions (testsuite/backup.test). The
    // actual `info_log!` emission happens on the main thread; see
    // `crate::pipeline::receiver::emit_backup_notice`.
    let file_rel = file_path
        .strip_prefix(&config.dest_dir)
        .unwrap_or(file_path)
        .to_path_buf();
    let backup_rel = backup_path
        .strip_prefix(&config.dest_dir)
        .unwrap_or(&backup_path)
        .to_path_buf();
    Ok(Some(BackupNotice {
        original: file_rel,
        backup: backup_rel,
    }))
}
