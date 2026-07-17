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

/// Sparse finalization carried from the write pass.
///
/// upstream: `fileio.c:43` `sparse_end()` - truncate the file to its logical
/// length (leaving the trailing region a hole) and punch the in-basis zero
/// runs so an `--inplace` update does not retain stale bytes.
pub(super) struct SparseFinalize {
    /// Logical end offset for `set_len` (`ftruncate`).
    pub(super) logical_len: u64,
    /// Absolute `(start, len)` ranges to punch out of the destination.
    pub(super) holes: Vec<(u64, u64)>,
}

/// Truncates `target` to the sparse logical length and punches its in-basis
/// zero runs. Runs before the file is put into place.
fn finalize_sparse(target: &Path, sparse: &SparseFinalize) -> io::Result<()> {
    let mut file = fs::OpenOptions::new().write(true).open(target)?;
    file.set_len(sparse.logical_len)?;
    for &(pos, len) in &sparse.holes {
        fast_io::punch_hole(&mut file, pos, len)?;
    }
    Ok(())
}

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
    sparse_final: Option<SparseFinalize>,
) -> io::Result<CommitOutcome> {
    // upstream: fileio.c:43 sparse_end() - for the temp+rename path, truncate
    // and punch the temp file before it is renamed into place. The inplace
    // path finalizes in its dedicated branch below (after any backup).
    if needs_rename {
        if let Some(ref sparse) = sparse_final {
            finalize_sparse(cleanup_guard.path(), sparse)?;
        }
    }

    // upstream: backup.c:make_backup() - rename existing file before overwrite.
    // With delay_updates, backup happens during the sweep, not here.
    //
    // The inplace case is deliberately excluded: under --inplace the destination
    // inode is rewritten in place, so a rename-to-backup here would move the very
    // file we already overwrote (its pre-transfer contents are gone by commit).
    // Upstream instead COPIES the pre-image aside BEFORE the inplace rewrite
    // (generator.c:1862,1898); oc mirrors that in `process_file` /
    // `process_whole_file` via `make_backup_copy` prior to the first write.
    let backup_notice = if !config.delay_updates && !begin.is_inplace {
        if let Some(ref backup_config) = config.backup {
            make_backup(&begin.file_path, backup_config, config)?
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
        let result = rename_config_sandboxed(config, cleanup_guard.path(), &staging_path)?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        cleanup_guard.keep();
        return Ok(CommitOutcome {
            was_copy: result,
            delayed_path: Some(staging_path),
            backup_notice,
        });
    }

    let was_copy = if needs_rename {
        let result = rename_config_sandboxed(config, cleanup_guard.path(), &begin.file_path)?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        result
    } else if begin.is_inplace {
        if let Some(ref sparse) = sparse_final {
            // upstream: fileio.c:47-52 sparse_end() - punch stale basis blocks
            // then ftruncate to the logical length for the in-place update.
            finalize_sparse(&begin.file_path, sparse)?;
        } else {
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
        }
        false
    } else {
        false
    };
    cleanup_guard.keep();
    // upstream: receiver.c:1035-1037 - once the file is committed, a basis that
    // came from the partial directory (FNAMECMP_PARTIAL_DIR) is unlinked and the
    // now-empty partial-dir is rmdir'd via handle_partial_dir(PDIR_DELETE). The
    // removal is unconditional for --partial-dir successes: when no partial
    // basis existed the unlink is a harmless no-op.
    remove_partial_dir_basis(config, &begin.file_path);
    Ok(CommitOutcome {
        was_copy,
        delayed_path: None,
        backup_notice,
    })
}

/// Removes the `--partial-dir` basis file after a successful commit and
/// rmdir's the (now-possibly-empty) partial directory for a relative
/// `--partial-dir`, mirroring upstream `handle_partial_dir(PDIR_DELETE)`.
///
/// Best-effort: a missing partial file or a non-empty partial-dir leaves the
/// filesystem untouched. Absolute `--partial-dir` values are never rmdir'd,
/// matching upstream `util1.c:1343` (`if (!create && *partial_dir == '/')`).
fn remove_partial_dir_basis(config: &DiskCommitConfig, dest_path: &Path) {
    let PartialMode::PartialDir(ref dir) = config.partial_mode else {
        return;
    };
    let Some(partial) = crate::temp_guard::partial_dir_fname(dest_path, dir) else {
        return;
    };
    let _ = fs::remove_file(&partial);
    // upstream: handle_partial_dir() only rmdir's a relative partial-dir.
    if !dir.is_absolute() {
        if let Some(parent) = partial.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
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
    config: &DiskCommitConfig,
    cleanup_guard: &mut TempFileGuard,
    dest_path: &Path,
) {
    match &config.partial_mode {
        PartialMode::None => {}
        PartialMode::Partial => {
            // upstream: cleanup.c:130-135 - rename temp file directly to
            // the destination. The incomplete content replaces any existing
            // file at the destination path.
            let temp_path = cleanup_guard.path().to_path_buf();
            match rename_config_sandboxed(config, cleanup_guard.path(), dest_path) {
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

/// SEC-1.j: dirfd-anchor the temp→final commit rename against the receiver's
/// destination sandbox, falling back to [`rename_with_io_uring_fallback`].
///
/// When the `config` carries a [`fast_io::DirSandbox`] rooted at its
/// `dest_dir`, and both the temp leaf and the final leaf are single components
/// directly under that root (the default in-destination temp pattern also used
/// by the temp-file create, see `temp_guard::try_create_new`), the rename routes
/// through [`fast_io::renameat_via_sandbox_or_fallback`]. Both leaves resolve
/// against the pinned destination dirfd, so a symlink swap on the commit parent
/// between temp-create and rename cannot redirect the final file outside the
/// tree.
///
/// In every other case (no sandbox, multi-component relative path, or a
/// `--temp-dir`/partial-dir on a different parent) it falls back to the existing
/// io_uring / `std::fs::rename` path with the EXDEV copy+remove backstop, so a
/// working rename is never regressed. The anchored path shares the destination
/// parent for both leaves, so EXDEV cannot arise there.
///
/// Returns `Ok(false)` for an in-place rename, `Ok(true)` when the EXDEV
/// copy+remove fallback ran.
#[cfg(unix)]
pub(super) fn rename_config_sandboxed(
    config: &DiskCommitConfig,
    old_path: &Path,
    new_path: &Path,
) -> io::Result<bool> {
    if let (Some(sandbox), Some(dest_dir)) = (config.sandbox.as_ref(), config.dest_dir.as_deref())
        && let (Some(old_leaf), Some(new_leaf)) = (old_path.file_name(), new_path.file_name())
        && old_path.parent() == Some(dest_dir)
        && new_path.parent() == Some(dest_dir)
    {
        // Both endpoints are single components under the sandbox root, so the
        // dirfd anchor applies. `replace = true` matches `fs::rename`'s
        // overwrite-the-destination semantics (upstream `do_rename`).
        fast_io::renameat_via_sandbox_or_fallback(
            Some(sandbox.as_ref()),
            dest_dir,
            Path::new(old_leaf),
            old_path,
            dest_dir,
            Path::new(new_leaf),
            new_path,
            true,
        )?;
        return Ok(false);
    }
    rename_with_io_uring_fallback(old_path, new_path)
}

/// Non-Unix: the `*at` sandbox helpers do not exist. On Windows the commit
/// rename routes through the reparse-point-anchored handle rename
/// ([`crate::temp_guard::commit_rename_no_follow`]), the counterpart to the
/// Unix `renameat` anchoring, so a junction/mount-point swap on the commit
/// parent between temp-create and rename cannot redirect the committed file
/// (CVE-2024-12747 residual). Other non-Unix targets keep the path-based
/// [`rename_with_io_uring_fallback`] with no behavior change.
#[cfg(not(unix))]
pub(super) fn rename_config_sandboxed(
    _config: &DiskCommitConfig,
    old_path: &Path,
    new_path: &Path,
) -> io::Result<bool> {
    #[cfg(windows)]
    {
        crate::temp_guard::commit_rename_no_follow(old_path, new_path)
    }
    #[cfg(not(windows))]
    {
        rename_with_io_uring_fallback(old_path, new_path)
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

/// SEC-1.j: dirfd-anchor the `--backup` rename against the receiver's
/// destination sandbox, otherwise `std::fs::rename`.
///
/// The backup rename moves an existing destination file to its backup name
/// (`file~` or `<backup-dir>/...`). When the sandbox is present and both the
/// original and the backup are single components directly under `dest_dir`, the
/// rename resolves both leaves against the pinned dirfd so a symlink swap on the
/// parent cannot redirect the backup outside the tree. Otherwise it falls back
/// to the original path-based [`std::fs::rename`] with no behavior change (no
/// io_uring/EXDEV path is introduced on the backup rename, matching prior
/// semantics).
#[cfg(unix)]
fn backup_rename_sandboxed(
    config: &DiskCommitConfig,
    old_path: &Path,
    new_path: &Path,
) -> io::Result<()> {
    if let (Some(sandbox), Some(dest_dir)) = (config.sandbox.as_ref(), config.dest_dir.as_deref())
        && let (Some(old_leaf), Some(new_leaf)) = (old_path.file_name(), new_path.file_name())
        && old_path.parent() == Some(dest_dir)
        && new_path.parent() == Some(dest_dir)
    {
        return fast_io::renameat_via_sandbox_or_fallback(
            Some(sandbox.as_ref()),
            dest_dir,
            Path::new(old_leaf),
            old_path,
            dest_dir,
            Path::new(new_leaf),
            new_path,
            true,
        );
    }
    fs::rename(old_path, new_path)
}

#[cfg(not(unix))]
fn backup_rename_sandboxed(
    _config: &DiskCommitConfig,
    old_path: &Path,
    new_path: &Path,
) -> io::Result<()> {
    fs::rename(old_path, new_path)
}

/// SEC-1.j: create the `--backup-dir` parent, dirfd-anchoring the leaf `mkdir`
/// against the receiver's destination sandbox when possible.
///
/// When the sandbox is present and `parent` is a single component directly
/// under `dest_dir`, the final directory component is created via
/// [`fast_io::mkdirat_via_sandbox_or_fallback`] so a symlink swap on the
/// destination root cannot redirect it. Deeper trees, or the no-sandbox case,
/// keep the original recursive [`std::fs::create_dir_all`] so behavior is
/// unchanged (`create_dir_all` is idempotent for already-existing ancestors).
#[cfg(unix)]
fn create_dir_all_sandboxed(config: &DiskCommitConfig, parent: &Path) -> io::Result<()> {
    if let (Some(sandbox), Some(dest_dir)) = (config.sandbox.as_ref(), config.dest_dir.as_deref())
        && parent.parent() == Some(dest_dir)
        && let Some(leaf) = parent.file_name()
    {
        return match fast_io::mkdirat_via_sandbox_or_fallback(
            Some(sandbox.as_ref()),
            dest_dir,
            Path::new(leaf),
            parent,
            0o777,
        ) {
            Ok(()) => Ok(()),
            // Match `create_dir_all`'s idempotence: an already-present dir is
            // not an error.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        };
    }
    fs::create_dir_all(parent)
}

#[cfg(not(unix))]
fn create_dir_all_sandboxed(_config: &DiskCommitConfig, parent: &Path) -> io::Result<()> {
    fs::create_dir_all(parent)
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
    backup_config: &BackupConfig,
    config: &DiskCommitConfig,
) -> io::Result<Option<BackupNotice>> {
    if !file_path.exists() {
        return Ok(None);
    }

    let backup_path = compute_backup_path(
        &backup_config.dest_dir,
        file_path,
        None,
        backup_config.backup_dir.as_deref(),
        &backup_config.suffix,
    );

    if let Some(parent) = backup_path.parent() {
        if !parent.exists() {
            create_dir_all_sandboxed(config, parent)?;
        }
    }

    backup_rename_sandboxed(config, file_path, &backup_path)?;
    // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1) on the RENAME success
    // branch of link_or_rename. disk_commit always uses rename here.
    trace_make_backup_rename(&file_path.display().to_string());
    // upstream: backup.c:352 - INFO_GTE(BACKUP, 1) fires on success label for
    // every successful backup. Paths are displayed relative to the destination
    // root to match upstream test assertions (testsuite/backup.test). The
    // actual `info_log!` emission happens on the main thread; see
    // `crate::pipeline::receiver::emit_backup_notice`.
    let file_rel = file_path
        .strip_prefix(&backup_config.dest_dir)
        .unwrap_or(file_path)
        .to_path_buf();
    let backup_rel = backup_path
        .strip_prefix(&backup_config.dest_dir)
        .unwrap_or(&backup_path)
        .to_path_buf();
    Ok(Some(BackupNotice {
        original: file_rel,
        backup: backup_rel,
    }))
}

/// Copies the destination's pre-transfer contents aside to the backup path,
/// used for the `--inplace --backup` case where the destination inode is
/// rewritten in place rather than replaced by a temp+rename.
///
/// upstream: backup.c make_backup() inplace copy path - the generator makes the
/// backup a COPY (`generator.c:1862` `copy_file(fname, backupptr, ...)`, and the
/// delta twin at `generator.c:1898`) BEFORE the receiver rewrites the
/// destination in place, keeping `fnamecmp_type == FNAMECMP_FNAME`. A plain
/// rename-to-backup would move the very inode we are about to update, so the
/// pre-image must be duplicated first. Unlike the rename path this does NOT emit
/// the `make_backup: RENAME` debug line (upstream's inplace copy bypasses
/// `make_backup()` and so emits no `DEBUG_GTE(BACKUP, 1)` trace), but it still
/// returns a [`BackupNotice`] so the main thread emits the same
/// `INFO_GTE(BACKUP, 1)` "backed up X to Y" line (`generator.c:1990-1992`).
///
/// Called before the first inplace write; the caller has already confirmed
/// `begin.is_inplace`. Returns `Ok(None)` when the destination does not yet
/// exist (nothing to back up), matching upstream's `x_lstat` guard.
pub(super) fn make_backup_copy(
    file_path: &Path,
    backup_config: &BackupConfig,
    config: &DiskCommitConfig,
) -> io::Result<Option<BackupNotice>> {
    if !file_path.exists() {
        return Ok(None);
    }

    let backup_path = compute_backup_path(
        &backup_config.dest_dir,
        file_path,
        None,
        backup_config.backup_dir.as_deref(),
        &backup_config.suffix,
    );

    if let Some(parent) = backup_path.parent() {
        if !parent.exists() {
            create_dir_all_sandboxed(config, parent)?;
        }
    }

    // upstream: generator.c:1866 copy_file() - duplicate the pre-transfer bytes
    // into the backup, leaving the original inode in place to be updated. A
    // pre-existing backup at this path is overwritten (upstream robust_unlinks
    // it at generator.c:1901); `fs::copy` truncates, reaching the same end
    // state. `fs::copy` is portable across Linux/macOS/Windows.
    fs::copy(file_path, &backup_path)?;

    // upstream: generator.c:1990-1992 - INFO_GTE(BACKUP, 1) "backed up X to Y".
    // Paths are relative to the destination root to match test assertions; the
    // `info_log!` emission happens on the main thread (see
    // `crate::pipeline::receiver::emit_backup_notice`).
    let file_rel = file_path
        .strip_prefix(&backup_config.dest_dir)
        .unwrap_or(file_path)
        .to_path_buf();
    let backup_rel = backup_path
        .strip_prefix(&backup_config.dest_dir)
        .unwrap_or(&backup_path)
        .to_path_buf();
    Ok(Some(BackupNotice {
        original: file_rel,
        backup: backup_rel,
    }))
}
