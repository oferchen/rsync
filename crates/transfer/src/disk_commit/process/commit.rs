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

use engine::{
    CleanupManager, clear_partial_dir_obstruction, compute_backup_path, copy_pre_image_to_backup,
    create_backup_dir_parents, trace_make_backup_copy, trace_make_backup_hlink,
    trace_make_backup_rename,
};

use crate::pipeline::messages::{BackupNotice, BeginMessage};
use crate::temp_guard::TempFileGuard;

use super::super::config::{BackupConfig, BackupEnv, DiskCommitConfig, PartialMode};

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
///
/// upstream: `fileio.c:43` `sparse_end()` runs inside `receive_data()` BEFORE
/// `receiver.c` calls `finish_transfer()` -> `set_file_attrs()`. Both `set_len`
/// (ftruncate) and `punch_hole` (fallocate) update the file mtime, so this must
/// run before the timestamp is applied or the just-set mtime is clobbered. The
/// temp+rename callers invoke it directly for that reason; the inplace branch
/// of `commit_file` re-applies metadata afterwards.
pub(super) fn finalize_sparse(target: &Path, sparse: &SparseFinalize) -> io::Result<()> {
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
    /// existing file. Propagated to the main thread via `CommitResult`
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
/// - `receiver.c:1285-1314`: delay_updates stages to partial dir
/// - `receiver.c:685-720`: `handle_delayed_updates()` bulk rename
pub(super) fn commit_file(
    begin: &BeginMessage,
    config: &DiskCommitConfig,
    cleanup_guard: &mut TempFileGuard,
    needs_rename: bool,
    bytes_written: u64,
    sparse_final: Option<SparseFinalize>,
) -> io::Result<CommitOutcome> {
    // upstream: fileio.c:43 sparse_end() - the temp+rename path truncates and
    // punches the temp file in the caller BEFORE applying metadata, so the
    // ftruncate/punch cannot re-stamp the mtime that set_file_attrs applied.
    // The inplace path finalizes in its dedicated branch below (after any
    // backup), then the caller re-applies metadata to the destination.

    // upstream: backup.c:make_backup() - rename existing file before overwrite.
    // With delay_updates, backup happens during the sweep, not here.
    //
    // The inplace case is deliberately excluded: under --inplace the destination
    // inode is rewritten in place, so a rename-to-backup here would move the very
    // file we already overwrote (its pre-transfer contents are gone by commit).
    // Upstream instead COPIES the pre-image aside BEFORE the inplace rewrite
    // (generator.c:2281,2328); oc mirrors that in `process_file` /
    // `process_whole_file` via `make_backup_copy` prior to the first write.
    let backup_notice = if !config.delay_updates && !begin.is_inplace {
        if let Some(ref backup_config) = config.backup {
            make_backup(&begin.file_path, backup_config, config.backup_env()).map_err(|e| {
                crate::temp_guard::attach_commit_op(
                    crate::temp_guard::CommitOp::Backup,
                    &begin.file_path,
                    e,
                )
            })?
        } else {
            None
        }
    } else {
        None
    };

    if needs_rename && config.delay_updates {
        if let Some(staging_path) = delay_updates_staging_path(config, &begin.file_path) {
            // upstream: util1.c:1518-1530 handle_partial_dir(..., PDIR_CREATE)
            // creates the partial directory before moving the temp into it,
            // and clears a non-directory standing at that name first
            // (:1523-1528). Without the clear, `create_dir_all` reports
            // `AlreadyExists` for that shape and the `?` fails the whole
            // transfer where upstream unlinks the obstruction and proceeds -
            // measured against real 3.5.0 over a daemon push, which exits 0.
            //
            // The create goes through [`create_dir_all_sandboxed`], the same
            // helper the `--backup-dir` parent already uses, rather than a bare
            // `std::fs::create_dir_all`. `.~tmp~` is a single component beneath
            // `dest_dir`, so that helper reduces it to one `mkdirat` anchored on
            // the destination sandbox. A bare `create_dir_all` reaches glibc's
            // `mkdir()` wrapper, which lowers to the legacy `mkdir(2)` on
            // x86_64 - a syscall the daemon worker's seccomp allowlist
            // deliberately omits in favour of the `*at` variants. MEASURED on
            // x86_64 CI: the whole `--delay-updates` push died here with
            // `Operation not permitted`, surfacing downstream as
            // `mkstemp ... failed`, while the identical push without
            // `--delay-updates` succeeded. aarch64 has no legacy `mkdir(2)` for
            // glibc to lower to, which is why the defect was architecture-only.
            if let Some(parent) = staging_path.parent() {
                clear_partial_dir_obstruction(parent)?;
                create_dir_all_sandboxed(config.backup_env(), parent)?;
            }
            let result = rename_config_sandboxed(config, cleanup_guard.path(), &staging_path)
                .map_err(|e| {
                    crate::temp_guard::attach_commit_op(
                        crate::temp_guard::CommitOp::Rename,
                        &staging_path,
                        e,
                    )
                })?;
            CleanupManager::global().unregister_temp_file(cleanup_guard.path());
            cleanup_guard.keep();
            return Ok(CommitOutcome {
                was_copy: result,
                delayed_path: Some(staging_path),
                backup_notice,
            });
        }
    }

    let was_copy = if needs_rename {
        let result = rename_config_sandboxed(config, cleanup_guard.path(), &begin.file_path)
            .map_err(|e| {
                crate::temp_guard::attach_commit_op(
                    crate::temp_guard::CommitOp::Rename,
                    &begin.file_path,
                    e,
                )
            })?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        result
    } else if begin.is_inplace && !begin.is_device_target {
        // upstream: receiver.c:652 gates the in-place ftruncate on
        // `!IS_DEVICE(file->mode)`, so `--write-devices` never truncates the
        // target device - its data lands via the in-place writes and a
        // block/char device has no length to set (ftruncate would fail EINVAL).
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
    // upstream: receiver.c:1291-1299 - once the file is committed, a basis that
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
/// filesystem untouched. The absolute-`--partial-dir` exemption belongs to
/// [`engine::remove_partial_dir`], which owns upstream `util1.c:1506-1507` for
/// both of oc's removal sites.
fn remove_partial_dir_basis(config: &DiskCommitConfig, dest_path: &Path) {
    let PartialMode::PartialDir(ref dir) = config.partial_mode else {
        return;
    };
    let Some(partial) = crate::temp_guard::partial_dir_fname(dest_path, dir) else {
        return;
    };
    let _ = fs::remove_file(&partial);
    engine::remove_partial_dir(Some(dir), &partial);
}

/// Resolves the `--delay-updates` staging path for a destination file.
///
/// upstream: receiver.c - under `--delay-updates` the received temp is
/// committed through `handle_partial_dir(partialptr, PDIR_CREATE)` instead of
/// being renamed onto the destination, where `partialptr` comes from
/// `partial_dir_fname(fname)`. The directory itself is whatever
/// `--partial-dir` names; `--delay-updates` alone is promoted to the implicit
/// `.~tmp~` by `TransferConfigBuilder::effective_partial_dir`
/// (upstream options.c:2563-2564), so the staging directory is read from the
/// configured partial mode rather than hardcoded here.
///
/// Returns `None` when the config carries no partial directory, which leaves
/// the caller on the ordinary rename-to-destination path.
pub(super) fn delay_updates_staging_path(
    config: &DiskCommitConfig,
    dest_path: &Path,
) -> Option<PathBuf> {
    let PartialMode::PartialDir(ref dir) = config.partial_mode else {
        return None;
    };
    crate::temp_guard::partial_dir_fname(dest_path, dir)
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
/// `zero_mtime` selects between the two upstream retention paths for plain
/// `--partial`:
///
/// - `true` (signal/abort cleanup): upstream `cleanup.c:174-180` zeros the
///   modtime (`cleanup_file->modtime = 0`, `tweak_modtime = 1`) so an
///   interrupted partial stands out in `ls` and is not skipped by `--update`.
///   Used by the interrupt paths (channel disconnect, `Abort`, `Shutdown`).
/// - `false` (normal failed-verify keep): upstream `receiver.c:1309` calls
///   `finish_transfer(..., recv_ok, ...)` with `recv_ok == 0`, which maps to
///   `ATTRS_SKIP_MTIME` (`rsync.c:911-912`), so the retained stub keeps its
///   recent temp-creation mtime rather than being reset to the epoch.
///
/// `--partial-dir` never zeros the mtime in either case (upstream routes it
/// through `handle_partial_dir()`, which leaves the timestamp alone).
///
/// # Upstream Reference
///
/// - `cleanup.c:169-170` - `handle_partial_dir()` moves temp to partial-dir
/// - `cleanup.c:174-180` - signal cleanup zeros modtime for plain `--partial`
/// - `receiver.c:1309` - normal keep uses `ok_to_set_time = recv_ok` (0 on fail)
/// - `rsync.c:911-912` - `ok_to_set_time ? ATTRS_ACCURATE_TIME : ATTRS_SKIP_MTIME`
pub(super) fn retain_partial_file(
    config: &DiskCommitConfig,
    cleanup_guard: &mut TempFileGuard,
    dest_path: &Path,
    zero_mtime: bool,
) {
    match &config.partial_mode {
        PartialMode::None => {}
        PartialMode::Partial => {
            // upstream: cleanup.c:181-182 - finish_transfer() puts the temp at
            // the destination. The incomplete content replaces any existing
            // file at the destination path.
            let temp_path = cleanup_guard.path().to_path_buf();
            match rename_config_sandboxed(config, cleanup_guard.path(), dest_path) {
                Ok(_) => {
                    // upstream: cleanup.c:174-180 - the signal/abort cleanup
                    // path stamps modtime=0 on the retained partial so it
                    // stands out as unfinished in an ls and --update does not
                    // skip it as "up to date". Only for plain --partial, not
                    // --partial-dir (handle_partial_dir() leaves the mtime
                    // alone). The normal failed-verify keep (receiver.c:1309,
                    // ok_to_set_time=0 -> ATTRS_SKIP_MTIME) does NOT zero it,
                    // preserving the recent temp-creation mtime - so zero only
                    // when `zero_mtime` is set (the interrupt paths).
                    //
                    // Use from_unix_time(0, 0) rather than FileTime::zero()
                    // because on Windows, zero() maps to the Windows epoch
                    // (1601-01-01) which becomes an all-zero FILETIME -
                    // SetFileTime treats that as "do not change", silently
                    // skipping the stamp. from_unix_time(0, 0) maps to
                    // 1970-01-01 which is a non-zero FILETIME that Windows
                    // will actually apply.
                    if zero_mtime {
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
            // upstream: cleanup.c:169-170 - move temp file into partial-dir
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
///
/// On Windows the commit rename routes through
/// `crate::temp_guard::commit_rename_no_follow` and the `#[cfg(not(windows))]`
/// arm of [`rename_config_sandboxed`] is compiled out, so this path-based
/// helper has no non-test caller there; the tests still exercise it.
#[cfg_attr(windows, allow(dead_code))]
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
/// When the `config` carries a [`fast_io::DirSandbox`] rooted at its `dest_dir`
/// and both endpoints live beneath that root, the rename routes through
/// [`fast_io::renameat_via_sandbox_or_fallback`]. A file directly under the
/// root resolves its leaf against the pinned destination dirfd; a file in a
/// destination subdirectory (`sub/file`, the common recursive-copy case) has
/// its parent opened under `openat2(RESOLVE_BENEATH)` inside the helper. Either
/// way a concurrent ancestor-symlink swap on the commit parent - including an
/// *interior* directory - between temp-create and rename cannot redirect the
/// final file outside the tree.
///
/// Anchoring the nested (subdir) case closes the CVE-2026-29518 secondary
/// residual on the pipelined disk-commit path: before this the subdir case fell
/// through to a path-based `std::fs::rename`, so on a privileged daemon
/// (`chroot=no`) a swapped interior directory could redirect the committed file
/// out of the module. This now matches both the non-pipelined receiver
/// (`transfer_ops::response.rs`) and the primary #6808 ownership/timestamp
/// anchoring.
///
/// upstream: `syscall.c:1866` `do_rename_at()` opens each slashed path's parent
/// via `secure_relative_open()` (openat2 `RESOLVE_BENEATH`) and issues
/// `renameat()` against the resulting dirfd, gated on `secure_relpath_active()`
/// (`syscall.c:100`).
///
/// In every other case (no sandbox, or a `--temp-dir`/partial-dir on a
/// different tree than `dest_dir`) it falls back to the existing io_uring /
/// `std::fs::rename` path with the EXDEV copy+remove backstop, so a working
/// rename is never regressed. The anchored path shares the destination subtree
/// for both endpoints, so EXDEV cannot arise there.
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
        && let (Ok(old_rel), Ok(new_rel)) = (
            old_path.strip_prefix(dest_dir),
            new_path.strip_prefix(dest_dir),
        )
    {
        // Both endpoints resolve beneath the sandbox root. The helper picks the
        // single-component dirfd fast path or the `RESOLVE_BENEATH` parent
        // anchor per endpoint, so a nested subdir commit is confined exactly
        // like a root-level one. `replace = true` matches `fs::rename`'s
        // overwrite-the-destination semantics (upstream `do_rename`).
        fast_io::renameat_via_sandbox_or_fallback(
            Some(sandbox.as_ref()),
            dest_dir,
            old_rel,
            old_path,
            dest_dir,
            new_rel,
            new_path,
            true,
        )?;
        return Ok(false);
    }
    rename_with_io_uring_fallback(old_path, new_path)
}

/// Non-Unix: the `*at` sandbox helpers do not exist. On Windows the commit
/// rename routes through the reparse-point-anchored handle rename
/// (`crate::temp_guard::commit_rename_no_follow`), the counterpart to the
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
/// Forwards to [`fast_io::is_cross_device`], the single source of truth shared
/// with the engine local-copy commit guard and [`crate::temp_guard`]. On Unix
/// this is `raw_os_error() == libc::EXDEV` (errno 18); on Windows
/// `ERROR_NOT_SAME_DEVICE` (error 17).
pub(super) fn is_cross_device(e: &io::Error) -> bool {
    fast_io::is_cross_device(e)
}

/// Moves an existing destination file to its backup path, falling back to a
/// copy when the rename cannot cross a filesystem boundary.
///
/// Returns `Ok(false)` when a plain rename moved the file and `Ok(true)` when
/// the cross-device (`EXDEV`) copy+unlink fallback ran - the case a
/// `--backup-dir` (or `--backup` suffix landing on another mount) on a
/// different filesystem than the destination triggers.
///
/// upstream: `backup.c:265` `make_backup_inner()` - after `link_or_rename()`
/// cannot move the pre-image across the mount (`do_rename_at` fails with
/// `EXDEV`), rsync falls back to `copy_file()` (`backup.c:401`), leaving the
/// original for the temp->destination rename to replace. oc reuses the same
/// `fs::copy` + `fs::remove_file` mechanism the tmp->dest commit uses in
/// [`rename_with_io_uring_fallback`] (`util1.c:robust_rename()`), so `fs::copy`
/// carries the mode bits exactly as upstream's `copy_file(..., file->mode)`.
/// Restores the pre-image's ownership, timestamps and mode onto a backup that
/// was COPIED rather than moved.
///
/// The hard-link and rename tiers of the backup ladder move or share the inode,
/// so its identity travels with it for free. The copy tier does not, which is
/// why upstream applies the attributes explicitly on that branch alone:
/// `set_file_attrs(buf, file, NULL, fname, ATTRS_ACCURATE_TIME)`
/// (`backup.c:420`), reached only from the `copy_file()` arm at `backup.c:400`.
/// Without it a cross-device `--backup-dir` keeps the bytes and the mode but
/// SILENTLY loses owner, group and both timestamps - the backup stops being a
/// faithful pre-image of what was replaced.
///
/// `source_meta` must be captured BEFORE the copy: the cross-device tier
/// unlinks the pre-image once the duplicate is in place, so a stat afterwards
/// would find nothing.
///
/// Best-effort, exactly like upstream: `set_file_attrs()` reports and continues
/// rather than failing the transfer, so a backup that cannot take the owner -
/// the ordinary non-root case - is still a valid backup. This mirrors the
/// sibling `apply_backup_dir_attrs()` on the engine's backup-parent path, and
/// the engine's own local-copy backup sites, which already do this.
///
/// The apply itself is confined: `metadata::apply` issues
/// `fast_io::secure_{chmod,chown,utimes}_at`, which re-resolve the parent per
/// call, so nothing here re-opens `backup_path` with libc path resolution.
fn apply_copied_backup_metadata(
    backup_path: &Path,
    source_meta: &fs::Metadata,
    env: BackupEnv<'_>,
) {
    let options = env.metadata_opts.cloned().unwrap_or_default();
    let _ = metadata::apply_file_metadata_with_options(backup_path, source_meta, &options);
}

fn backup_rename_or_copy(old_path: &Path, new_path: &Path, env: BackupEnv<'_>) -> io::Result<bool> {
    match backup_rename_syscall(old_path, new_path) {
        Ok(()) => Ok(false),
        Err(e) if is_cross_device(&e) => {
            // upstream: backup.c:400-416 - the keep_backup copy tier duplicates
            // the pre-image with copy_file(); oc unlinks the source afterwards so
            // the backup ends up on the other filesystem.
            //
            // Stat BEFORE the copy: `remove_file` below destroys the pre-image,
            // and `fs::copy` carries only content and permission bits, so the
            // owner and timestamps have to be read while the inode still exists.
            let source_meta = fs::symlink_metadata(old_path)?;
            fs::copy(old_path, new_path)?;
            // upstream: backup.c:420 set_file_attrs() on the copy branch.
            apply_copied_backup_metadata(new_path, &source_meta, env);
            fs::remove_file(old_path)?;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

/// Issues the backup rename through the operator-path ownership walk, bound to
/// the session's confinement root.
///
/// This is the tier that runs for every `--backup-dir`, because the SEC-1.j
/// dirfd anchoring above only covers a backup name that is a single component
/// directly under the destination root. A bare `fs::rename` here follows a
/// directory symlink standing at the `--backup-dir`, and a non-chrooted daemon
/// owns everything it creates, so such a symlink is TRUSTED-owned by
/// construction: the destination's pre-transfer bytes are then MOVED out of the
/// module and the client still exits 0.
///
/// upstream: `backup.c:443-449` `make_backup()` sets `operator_path_resolve`
/// around the whole backup, and `syscall.c:1891` `do_rename_at()` walks each
/// side with `owner_walk_parent()` while it is set. A session with no
/// confinement root - every plain local or remote-shell client - has nothing to
/// be outside of, so only the ownership half applies there.
///
/// Under `cfg(test)` a fault-injection guard can force a cross-device (`EXDEV`)
/// error so the copy fallback in [`backup_rename_or_copy`] can be exercised
/// deterministically on filesystems that would otherwise complete the rename.
#[cfg(not(test))]
#[inline]
fn backup_rename_syscall(old_path: &Path, new_path: &Path) -> io::Result<()> {
    backup_rename_confined(old_path, new_path)
}

#[cfg(test)]
fn backup_rename_syscall(old_path: &Path, new_path: &Path) -> io::Result<()> {
    if force_exdev_active() {
        return Err(simulated_cross_device_error());
    }
    backup_rename_confined(old_path, new_path)
}

#[cfg(unix)]
#[inline]
fn backup_rename_confined(old_path: &Path, new_path: &Path) -> io::Result<()> {
    fast_io::operator_rename_confined(old_path, new_path, true)
}

#[cfg(not(unix))]
#[inline]
fn backup_rename_confined(old_path: &Path, new_path: &Path) -> io::Result<()> {
    fs::rename(old_path, new_path)
}

// Test-only fault injection for the backup rename boundary. A thread-local flag
// (nextest runs each test in its own process, and the guard is thread-scoped
// regardless) makes `backup_rename_syscall` report a cross-device error,
// driving the EXDEV copy+remove path without a real second filesystem.
#[cfg(test)]
thread_local! {
    static FORCE_EXDEV: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn force_exdev_active() -> bool {
    FORCE_EXDEV.with(std::cell::Cell::get)
}

/// Builds an error `is_cross_device` recognizes on the current platform.
#[cfg(test)]
fn simulated_cross_device_error() -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        io::Error::from_raw_os_error(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        io::Error::other("simulated cross-device")
    }
}

/// Test-only RAII guard that forces [`backup_rename_syscall`] onto the
/// cross-device path for its lifetime, exercising the copy fallback.
#[cfg(test)]
pub(crate) struct ForceExdev;

#[cfg(test)]
impl ForceExdev {
    pub(crate) fn new() -> Self {
        FORCE_EXDEV.with(|c| c.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for ForceExdev {
    fn drop(&mut self) {
        FORCE_EXDEV.with(|c| c.set(false));
    }
}

/// SEC-1.j: dirfd-anchor the `--backup` rename against the receiver's
/// destination sandbox, otherwise rename with a cross-device copy fallback.
///
/// The backup rename moves an existing destination file to its backup name
/// (`file~` or `<backup-dir>/...`). When the sandbox is present and both the
/// original and the backup are single components directly under `dest_dir`, the
/// rename resolves both leaves against the pinned dirfd so a symlink swap on the
/// parent cannot redirect the backup outside the tree; both endpoints then
/// share the destination filesystem, so `EXDEV` cannot arise and the call
/// returns `Ok(false)`.
///
/// Otherwise - the common `--backup-dir` case, where the backup tree may live
/// on a different mount than the destination - it falls back to
/// [`backup_rename_or_copy`], which renames and, on a cross-device failure,
/// copies the pre-image and unlinks the original (upstream `backup.c:265`).
/// Returns `Ok(true)` when that copy fallback ran so the caller can emit the
/// upstream `make_backup: COPY` trace instead of `RENAME`.
#[cfg(unix)]
fn backup_rename_sandboxed(
    env: BackupEnv<'_>,
    old_path: &Path,
    new_path: &Path,
) -> io::Result<bool> {
    if let (Some(sandbox), Some(dest_dir)) = (env.sandbox, env.dest_dir)
        && let (Some(old_leaf), Some(new_leaf)) = (old_path.file_name(), new_path.file_name())
        && old_path.parent() == Some(dest_dir)
        && new_path.parent() == Some(dest_dir)
    {
        fast_io::renameat_via_sandbox_or_fallback(
            Some(sandbox),
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
    backup_rename_or_copy(old_path, new_path, env)
}

#[cfg(not(unix))]
fn backup_rename_sandboxed(
    env: BackupEnv<'_>,
    old_path: &Path,
    new_path: &Path,
) -> io::Result<bool> {
    backup_rename_or_copy(old_path, new_path, env)
}

/// Issues the backup hard link, dirfd-anchoring the backup endpoint against the
/// receiver's destination sandbox when the backup name is a single component
/// under `dest_dir` (SEC-1.h shape, same as the hardlink-follower create).
///
/// Every other shape - which is every `--backup-dir`, whose backup name carries
/// the directory as a second component - takes the operator-path ownership walk
/// bound to the session's confinement root, the same resolver
/// [`backup_rename_syscall`] uses one tier down. The two tiers have to agree:
/// upstream raises `operator_path_resolve` around `link_or_rename()` as a
/// whole, and the link tier runs FIRST, so confining only the rename leaves the
/// escape wide open on any platform where the link succeeds.
///
/// Under `cfg(test)` the `ForceExdev` guard makes this report a cross-device
/// error, matching a real `--backup-dir` on another mount where `link(2)` fails
/// with `EXDEV` before `rename(2)` does.
///
/// upstream: `backup.c:443-449` `make_backup()`; `backup.c:239-246`
/// `link_or_rename()`; `syscall.c:961` `do_link_at()` under
/// `operator_path_resolve`.
fn backup_hardlink_syscall(env: BackupEnv<'_>, old_path: &Path, new_path: &Path) -> io::Result<()> {
    #[cfg(test)]
    if force_exdev_active() {
        return Err(simulated_cross_device_error());
    }
    #[cfg(unix)]
    {
        if let (Some(sandbox), Some(dest_dir)) = (env.sandbox, env.dest_dir)
            && new_path.parent() == Some(dest_dir)
        {
            return fast_io::linkat_via_sandbox_or_fallback(
                Some(sandbox),
                old_path,
                dest_dir,
                new_path.strip_prefix(dest_dir).unwrap_or(new_path),
                new_path,
            );
        }
        fast_io::operator_link_confined(old_path, new_path)
    }
    #[cfg(not(unix))]
    {
        let _ = env;
        fast_io::hard_link(old_path, new_path)
    }
}

/// Upstream's hard-link tier of `link_or_rename()` with `prefer_rename = 0`.
///
/// Returns `true` when the pre-image was duplicated into the backup area as a
/// second link - upstream's `ret == 2`, which leaves the original in place for
/// the temp->destination rename to replace. Any failure returns `false` so the
/// caller falls through to the unchanged rename/copy tier.
///
/// upstream: `backup.c:239-246` - `do_link_at` success traces HLINK and returns
/// 2; a link failure on a regular file falls through to `do_rename_at`.
/// upstream: `backup.c:318-327` `make_backup_inner()` - an `EEXIST`/`EISDIR`
/// collision deletes the stale backup entry and retries `link_or_rename` once.
fn backup_hardlink_tier(env: BackupEnv<'_>, old_path: &Path, new_path: &Path) -> bool {
    match backup_hardlink_syscall(env, old_path, new_path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(new_path).is_ok()
                && backup_hardlink_syscall(env, old_path, new_path).is_ok()
        }
        Err(_) => false,
    }
}

/// Creates the parent directories of a backup path.
///
/// With `--backup-dir`, each freshly-created subdirectory inherits its
/// corresponding destination directory's attributes and any non-directory
/// obstruction is removed, mirroring upstream `copy_valid_path`
/// (backup.c:88-190) via the shared [`create_backup_dir_parents`] helper; the
/// leaf `mkdir` stays SEC-1.j sandbox-anchored through the injected
/// [`create_dir_all_sandboxed`]. Without `--backup-dir` the backup lands
/// alongside the destination, whose parent already exists, so upstream runs no
/// `copy_valid_path` (backup.c:195) and a plain create suffices.
fn create_backup_path_parents(
    parent: &Path,
    backup_config: &BackupConfig,
    env: BackupEnv<'_>,
) -> io::Result<()> {
    match backup_config.backup_dir.as_deref() {
        Some(backup_dir) => {
            let metadata_opts = env.metadata_opts.cloned().unwrap_or_default();
            create_backup_dir_parents(
                &backup_config.dest_dir,
                backup_dir,
                parent,
                &metadata_opts,
                |path| create_dir_all_sandboxed(env, path),
            )
        }
        None if parent.exists() => Ok(()),
        None => create_dir_all_sandboxed(env, parent),
    }
}

/// SEC-1.j: create the `--backup-dir` parent, dirfd-anchoring the leaf `mkdir`
/// against the receiver's destination sandbox when possible.
///
/// When the sandbox is present and `parent` is a single component directly
/// under `dest_dir`, the final directory component is created via
/// [`fast_io::mkdirat_via_sandbox_or_fallback`] so a symlink swap on the
/// destination root cannot redirect it. Deeper trees, and the no-sandbox case,
/// go through [`fast_io::operator_create_dir_all`], which is recursive like
/// `std::fs::create_dir_all` but issues `mkdirat` per component.
#[cfg(unix)]
fn create_dir_all_sandboxed(env: BackupEnv<'_>, parent: &Path) -> io::Result<()> {
    if let (Some(sandbox), Some(dest_dir)) = (env.sandbox, env.dest_dir)
        && parent.parent() == Some(dest_dir)
        && let Some(leaf) = parent.file_name()
    {
        return match fast_io::mkdirat_via_sandbox_or_fallback(
            Some(sandbox),
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
    fast_io::operator_create_dir_all(parent, 0o777)
}

#[cfg(not(unix))]
fn create_dir_all_sandboxed(_env: BackupEnv<'_>, parent: &Path) -> io::Result<()> {
    fs::create_dir_all(parent)
}

/// Creates a backup of the destination file before overwriting.
///
/// Mirrors upstream `backup.c:make_backup()`, called from `rsync.c:897`
/// `finish_transfer()` as `make_backup(fname, False)`: `link_or_rename()` tries
/// `do_link_at` first (HLINK, upstream `ret == 2`, the original stays in place
/// for the temp->destination rename to replace) and only falls back to
/// `do_rename_at` (RENAME) or the cross-device copy tier (COPY) when the link
/// cannot be made. Parent directories are created if needed when using
/// `--backup-dir`. On success, emits the matching `--debug=BACKUP` mechanism
/// notice (`backup.c:240-241`/`:255-256`/`:414`) and returns a [`BackupNotice`] carrying the
/// destination-relative paths so the main thread can emit upstream's
/// `INFO_GTE(BACKUP, 1)` line (`backup.c:432`). The disk thread cannot emit
/// the info line directly because its thread-local [`logging::VerbosityConfig`]
/// is never seeded with the user's `--info=backup` selection.
pub(crate) fn make_backup(
    file_path: &Path,
    backup_config: &BackupConfig,
    env: BackupEnv<'_>,
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
        create_backup_path_parents(parent, backup_config, env)?;
    }

    // upstream: backup.c:230-246 - `prefer_rename` is False here (rsync.c:897),
    // so `do_link_at` runs first. The tier is limited to a regular pre-image:
    // upstream gates symlinks and specials on CAN_HARDLINK_SYMLINK /
    // CAN_HARDLINK_SPECIAL (backup.c:231-238), and this commit path only ever
    // replaces a regular destination anyway.
    let is_regular = fs::symlink_metadata(file_path).is_ok_and(|m| m.is_file());
    if is_regular && backup_hardlink_tier(env, file_path, &backup_path) {
        // upstream: backup.c:240-241 - DEBUG_GTE(BACKUP, 1) HLINK success. The
        // original is deliberately left in place (upstream returns 2 and
        // finish_transfer does not redirect fnamecmp); the temp->destination
        // rename that follows replaces it.
        trace_make_backup_hlink(&file_path.display().to_string());
        return Ok(Some(backup_notice(backup_config, file_path, &backup_path)));
    }

    let was_copy = backup_rename_sandboxed(env, file_path, &backup_path)?;
    if was_copy {
        // upstream: backup.c:414 - DEBUG_GTE(BACKUP, 1) "make_backup: COPY %s
        // successful." when the cross-device copy tier moved the pre-image.
        trace_make_backup_copy(&file_path.display().to_string());
    } else {
        // upstream: backup.c:255-256 - DEBUG_GTE(BACKUP, 1) on the RENAME success
        // branch of link_or_rename.
        trace_make_backup_rename(&file_path.display().to_string());
    }
    Ok(Some(backup_notice(backup_config, file_path, &backup_path)))
}

/// Builds the destination-relative [`BackupNotice`] for a completed backup.
///
/// upstream: `backup.c:432` - INFO_GTE(BACKUP, 1) fires on the `success:` label
/// for every successful backup, whichever mechanism ran. Paths are displayed
/// relative to the destination root to match upstream test assertions
/// (`testsuite/backup.test`). The actual `info_log!` emission happens on the
/// main thread; see `crate::pipeline::receiver::emit_backup_notice`.
fn backup_notice(
    backup_config: &BackupConfig,
    file_path: &Path,
    backup_path: &Path,
) -> BackupNotice {
    BackupNotice {
        original: file_path
            .strip_prefix(&backup_config.dest_dir)
            .unwrap_or(file_path)
            .to_path_buf(),
        backup: backup_path
            .strip_prefix(&backup_config.dest_dir)
            .unwrap_or(backup_path)
            .to_path_buf(),
    }
}

/// Copies the destination's pre-transfer contents aside to the backup path,
/// used for the `--inplace --backup` case where the destination inode is
/// rewritten in place rather than replaced by a temp+rename.
///
/// upstream: backup.c make_backup() inplace copy path - the generator makes the
/// backup a COPY (`generator.c:2281` `copy_file(fname, backupptr, ...)`, and the
/// delta twin at `generator.c:2328`) BEFORE the receiver rewrites the
/// destination in place, keeping `fnamecmp_type == FNAMECMP_FNAME`. A plain
/// rename-to-backup would move the very inode we are about to update, so the
/// pre-image must be duplicated first. Unlike the rename path this does NOT emit
/// the `make_backup: RENAME` debug line (upstream's inplace copy bypasses
/// `make_backup()` and so emits no `DEBUG_GTE(BACKUP, 1)` trace), but it still
/// returns a [`BackupNotice`] so the main thread emits the same
/// `INFO_GTE(BACKUP, 1)` "backed up X to Y" line (`generator.c:2448-2450`).
///
/// Called before the first inplace write; the caller has already confirmed
/// `begin.is_inplace`. Returns `Ok(None)` when the destination does not yet
/// exist (nothing to back up), matching upstream's `x_lstat` guard.
pub(super) fn make_backup_copy(
    file_path: &Path,
    backup_config: &BackupConfig,
    env: BackupEnv<'_>,
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
        create_backup_path_parents(parent, backup_config, env)?;
    }

    // upstream: generator.c:2295 copy_file() - duplicate the pre-transfer bytes
    // into the backup, leaving the original inode in place to be updated. A
    // pre-existing backup at this path is overwritten (upstream robust_unlinks
    // it at generator.c:2340); the O_TRUNC create reaches the same end state.
    // The backup path is operator-named and resolves through the ownership
    // walk: generator.c:2283 raises `operator_path_resolve` around this copy
    // precisely because the in-place backup bypasses `make_backup()`.
    // Stat before the copy so the read is of the pre-image, not of whatever a
    // concurrent write may leave behind afterwards.
    let source_meta = fs::symlink_metadata(file_path)?;
    copy_pre_image_to_backup(file_path, &backup_path)?;
    // upstream: generator.c:2448 set_file_attrs(backupptr, back_file, ...) - the
    // in-place backup is a COPY, so it carries no inode identity of its own.
    apply_copied_backup_metadata(&backup_path, &source_meta, env);

    // upstream: generator.c:2448-2450 - INFO_GTE(BACKUP, 1) "backed up X to Y".
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
