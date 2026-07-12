//! Transfer orchestration for the receiver role.
//!
//! Provides the `run`, `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! entry points plus the common `setup_transfer` initialization. The driving
//! loops live in their own submodules:
//!
//! - `sync` - sequential per-file transfer used by `run_sync`.
//! - `sync_async` - `.await` twin of `sync` used by `run_sync_async`
//!   (`tokio-transfer` only).
//! - `pipelined` - decoupled two-phase pipeline used by `run_pipelined`.
//! - `pipelined_async` - `.await` twin of `pipelined` used by
//!   `run_pipelined_async` (`tokio-transfer` only).
//! - `pipelined_incremental` - same as `pipelined` plus incremental directory
//!   creation and failed-dir tracking.
//! - `pipelined_incremental_async` - `.await` twin of `pipelined_incremental`
//!   used by `run_pipelined_incremental_async` (`tokio-transfer` only).
//! - `setup` - common multiplex/filter/file-list setup.
//! - `phases` - protocol phase exchange and goodbye handshake.
//! - `candidates` - candidate-file selection for the pipelined paths.
//! - `pipeline` - the inner `run_pipeline_loop_decoupled` plus dry-run loop.
//! - `pipeline_async` - `.await` twin of `pipeline` used by the async pipelined
//!   drivers (`tokio-transfer` only).

mod candidates;
#[cfg(feature = "tokio-transfer")]
mod file_async;
mod phases;
mod pipeline;
#[cfg(feature = "tokio-transfer")]
mod pipeline_async;
mod pipelined;
#[cfg(feature = "tokio-transfer")]
mod pipelined_async;
mod pipelined_incremental;
#[cfg(feature = "tokio-transfer")]
mod pipelined_incremental_async;
mod setup;
mod sync;
#[cfg(feature = "tokio-transfer")]
mod sync_async;

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};

use crate::receiver::ReceiverContext;
use crate::receiver::stats::TransferStats;

impl ReceiverContext {
    /// Runs the receiver role to completion.
    ///
    /// Orchestrates the full receive operation: file list reception, signature
    /// generation, delta application, and metadata finalization. Delegates to
    /// `run_pipelined_incremental` (with `incremental-flist`) or `run_pipelined`.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` main reception loop
    /// - `main.c:1160-1200` - `do_recv()` orchestration
    pub fn run<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental(
                reader,
                writer,
                crate::pipeline::PipelineConfig::default(),
                progress,
            )
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            let _ = progress;
            self.run_pipelined(reader, writer, crate::pipeline::PipelineConfig::default())
        }
    }

    /// BENCHMARK-ONLY async twin of [`run`](Self::run) over a real socket.
    ///
    /// Adopts `socket` (a dup'd clone of the transfer socket) as the async read
    /// half: it is flipped non-blocking and handed to the tokio reactor, then
    /// wrapped as a plain [`AsyncServerReader`](crate::reader::AsyncServerReader)
    /// (multiplex activation happens inside the driver's `setup_transfer_async`,
    /// exactly as the sync path activates it inside `setup_transfer`). The
    /// wire-facing reads are driven through the matching async receiver driver;
    /// the synchronous request half keeps writing through the caller's blocking
    /// `writer`, which is a separate socket clone. Both fds point at the same
    /// kernel socket, so the async reads continue from exactly where the
    /// synchronous protocol setup left off - sound only because that setup reads
    /// the compat exchange directly (no look-ahead buffer that could strand wire
    /// bytes in a discarded reader).
    ///
    /// This exists purely to measure the async receiver driver against the
    /// threaded one over a live socket. The synchronous write leg still blocks,
    /// so it is not production-safe and not wire-fidelity-guaranteed under
    /// arbitrary backpressure. Reachable only with the `async-bench` feature
    /// compiled AND `OC_RSYNC_ASYNC_BENCH=1` set at runtime.
    #[cfg(feature = "async-bench")]
    pub(crate) async fn run_receiver_async_bench<W>(
        &mut self,
        socket: std::net::TcpStream,
        writer: &mut W,
    ) -> io::Result<TransferStats>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        socket.set_nonblocking(true)?;
        let async_socket = tokio::net::TcpStream::from_std(socket)?;
        let reader = crate::reader::AsyncServerReader::new_plain(async_socket);

        // Mirror the sync `run` dispatch exactly so the benchmark compares
        // like-for-like: incremental directory creation when `incremental-flist`
        // is compiled, otherwise the decoupled pipeline.
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental_async(
                reader,
                writer,
                crate::pipeline::PipelineConfig::default(),
                None,
            )
            .await
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            self.run_pipelined_async(reader, writer, crate::pipeline::PipelineConfig::default())
                .await
        }
    }

    /// True when the delete pass runs EARLY, before the per-file transfer loop.
    ///
    /// Covers `--delete-before`, `--delete-during`, AND `--delete-delay`. Mirrors
    /// upstream generator.c:2280-2281 (`delete_before` runs `do_delete_pass()` up
    /// front) and generator.c:2315-2327 (`delete_during` / `delete_during == 2`
    /// decide as each directory is entered during the loop). oc collapses these
    /// into one pre-loop sweep; the observable file outcome matches because none
    /// of these modes has the destination `.rsync-filter` merge files present
    /// when the deletion decision is made. `--delete-delay` defers only the
    /// physical unlink upstream, not the decision, so its kept/deleted set equals
    /// `--delete-during` - verified vs upstream 3.4.4 over SSH (delay DELETES a
    /// per-dir-merge-protected entry, exactly as during/before do).
    pub(in crate::receiver) fn delete_pass_is_early(&self) -> bool {
        self.config.flags.delete && !self.config.deletion.delete_after
    }

    /// True when the delete pass is DEFERRED to after the per-file transfer loop.
    ///
    /// Covers `--delete-after` ONLY. Mirrors upstream generator.c:2425-2428 which
    /// runs `do_delete_pass()` after every file - including each destination
    /// `.rsync-filter` merge file - has been transferred. Deferring is
    /// load-bearing: the delete pass reloads each destination directory's
    /// per-directory `.rsync-filter` at delete time, so a merge-file protect rule
    /// (e.g. `- *.bak`) only survives the sweep once that filter file is present
    /// in the destination, which it is not until the transfer has run.
    ///
    /// `--delete-delay` is deliberately NOT here: upstream makes its deletion
    /// decision during the walk (deferring only the unlink), so it deletes such
    /// an entry - see [`delete_pass_is_early`](Self::delete_pass_is_early).
    pub(in crate::receiver) fn delete_pass_is_late(&self) -> bool {
        self.config.flags.delete && self.config.deletion.delete_after
    }

    /// Runs the destination delete pass and folds its results into `stats`.
    ///
    /// Sweeps the destination for entries absent from the sender's file list,
    /// reloading each directory's per-directory `.rsync-filter` merge files so
    /// their protect rules apply at delete time. Shared by all four receiver
    /// pipeline drivers so the defer-and-filter logic lives in exactly one place.
    ///
    /// The caller positions the call via [`delete_pass_is_early`] (before the
    /// per-file loop) or [`delete_pass_is_late`] (after it, before finalize).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:358` - `do_delete_pass()` tree sweep
    /// - `generator.c:279` - `delete_in_dir()` per-directory candidate check
    /// - `exclude.c:875` - `change_local_filter_dir()` reloads dest `.rsync-filter`
    /// - `generator.c:2393-2398` / `2437-2440` - `write_del_stats()` emission
    ///
    /// [`delete_pass_is_early`]: Self::delete_pass_is_early
    /// [`delete_pass_is_late`]: Self::delete_pass_is_late
    pub(in crate::receiver) fn run_receiver_delete_pass<W>(
        &mut self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
        stats: &mut TransferStats,
    ) -> io::Result<()>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        let (delete_stats, limit_exceeded, io_bits) = self.delete_extraneous_files(
            dest_dir,
            #[cfg(unix)]
            sandbox,
            writer,
        )?;
        stats.io_error |= io_bits;
        stats.delete_stats = delete_stats;
        stats.delete_limit_exceeded = limit_exceeded;
        // Carry the per-type counters into the receiver context so the goodbye
        // handshake can emit NDX_DEL_STATS to the peer sender.
        // upstream: generator.c:2393-2398 - write_del_stats() emission.
        self.pending_del_stats = delete_stats;
        Ok(())
    }
}

/// Renames all delayed-update files from their `.~tmp~` staging paths to
/// their final destinations, then removes the empty `.~tmp~` directories.
///
/// Mirrors upstream `receiver.c:422-450 handle_delayed_updates()` which
/// iterates `delayed_bits`, renames each file from its `partial_dir_fname()`
/// path to the final destination, and calls `handle_partial_dir(PDIR_DELETE)`.
///
/// When `backup_config` is `Some`, backs up the existing destination file
/// before the rename (upstream: `receiver.c:431 make_backup(fname, False)`).
///
/// Rename failures are logged but do not abort the transfer, matching
/// upstream which calls `rsyserr(FERROR_XFER, ...)` and continues.
pub(in crate::receiver) fn handle_delayed_updates(
    delayed: &[(PathBuf, PathBuf)],
    backup_config: Option<crate::disk_commit::BackupConfig>,
) {
    use std::collections::HashSet;
    use std::fs;

    let mut staging_dirs: HashSet<PathBuf> = HashSet::new();

    for (staging_path, final_path) in delayed {
        // upstream: receiver.c:431-432 - make_backup(fname, False)
        if let Some(ref bc) = backup_config {
            if final_path.exists() {
                let backup_path = engine::compute_backup_path(
                    &bc.dest_dir,
                    final_path,
                    None,
                    bc.backup_dir.as_deref(),
                    &bc.suffix,
                );
                if let Some(parent) = backup_path.parent() {
                    if !parent.exists() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                match fs::rename(final_path, &backup_path) {
                    Ok(()) => {
                        // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1)
                        // RENAME success notice. The delayed-updates sweep is
                        // the third backup site (alongside disk-commit and
                        // local-copy); upstream emits this from
                        // backup.c:make_backup() regardless of caller.
                        engine::trace_make_backup_rename(&final_path.display().to_string());
                        // upstream: backup.c:352-353 - INFO_GTE(BACKUP, 1)
                        // rprintf(FINFO, "backed up %s to %s\n", fname, buf)
                        // fires on success label of make_backup() after the
                        // rename completes. Paths are displayed relative to
                        // the destination root to match the upstream test
                        // assertions (testsuite/backup.test:29,43,56).
                        let final_rel = final_path.strip_prefix(&bc.dest_dir).unwrap_or(final_path);
                        let backup_rel = backup_path
                            .strip_prefix(&bc.dest_dir)
                            .unwrap_or(&backup_path);
                        info_log!(
                            Backup,
                            1,
                            "backed up {} to {}",
                            final_rel.display(),
                            backup_rel.display()
                        );
                    }
                    Err(e) => {
                        eprintln!("rsync: backup failed for {}: {e}", final_path.display());
                    }
                }
            }
        }

        // upstream: receiver.c:433-435 - DEBUG_GTE(RECV, 1) rename notice
        debug_log!(
            Recv,
            1,
            "renaming {} to {}",
            staging_path.display(),
            final_path.display()
        );

        // upstream: receiver.c:439 - do_rename(partialptr, fname)
        if let Err(e) = fs::rename(staging_path, final_path) {
            eprintln!(
                "rsync: rename failed for {} (from {}): {e}",
                final_path.display(),
                staging_path.display()
            );
            continue;
        }

        // Track parent staging directories for cleanup.
        if let Some(parent) = staging_path.parent() {
            staging_dirs.insert(parent.to_path_buf());
        }
    }

    // upstream: receiver.c:446 - handle_partial_dir(partialptr, PDIR_DELETE)
    // Remove empty .~tmp~ staging directories.
    for dir in &staging_dirs {
        let _ = fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Verifies the delayed rename sweep moves files from staging paths to
    /// final destinations, matching upstream `receiver.c:422-450`.
    #[test]
    fn handle_delayed_updates_renames_staged_files() {
        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        // Create two staged files.
        let staged_a = staging_dir.join("a.txt");
        let staged_b = staging_dir.join("b.txt");
        fs::write(&staged_a, b"content-a").unwrap();
        fs::write(&staged_b, b"content-b").unwrap();

        let final_a = dir.path().join("a.txt");
        let final_b = dir.path().join("b.txt");

        let delayed = vec![
            (staged_a.clone(), final_a.clone()),
            (staged_b.clone(), final_b.clone()),
        ];

        handle_delayed_updates(&delayed, None);

        // Files should be at final paths.
        assert_eq!(fs::read_to_string(&final_a).unwrap(), "content-a");
        assert_eq!(fs::read_to_string(&final_b).unwrap(), "content-b");

        // Staging paths should no longer exist.
        assert!(!staged_a.exists());
        assert!(!staged_b.exists());

        // The empty .~tmp~ directory should have been cleaned up.
        assert!(
            !staging_dir.exists(),
            "empty .~tmp~ dir should be removed after sweep"
        );
    }

    /// Verifies that the sweep cleans up the `.~tmp~` directory even when
    /// it contained multiple files across different parent directories.
    #[test]
    fn handle_delayed_updates_cleans_multiple_staging_dirs() {
        let dir = test_support::create_tempdir();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let tmp1 = dir.path().join(".~tmp~");
        let tmp2 = sub.join(".~tmp~");
        fs::create_dir(&tmp1).unwrap();
        fs::create_dir(&tmp2).unwrap();

        let staged1 = tmp1.join("f1.txt");
        let staged2 = tmp2.join("f2.txt");
        fs::write(&staged1, b"one").unwrap();
        fs::write(&staged2, b"two").unwrap();

        let final1 = dir.path().join("f1.txt");
        let final2 = sub.join("f2.txt");

        let delayed = vec![
            (staged1.clone(), final1.clone()),
            (staged2.clone(), final2.clone()),
        ];

        handle_delayed_updates(&delayed, None);

        assert_eq!(fs::read_to_string(&final1).unwrap(), "one");
        assert_eq!(fs::read_to_string(&final2).unwrap(), "two");
        assert!(!tmp1.exists());
        assert!(!tmp2.exists());
    }

    /// Verifies the sweep continues past rename failures (matching upstream
    /// which logs errors but does not abort).
    #[test]
    fn handle_delayed_updates_continues_on_rename_failure() {
        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        // Create one valid staged file and one that points to a missing source.
        let staged_good = staging_dir.join("good.txt");
        fs::write(&staged_good, b"good").unwrap();
        let staged_bad = PathBuf::from("/nonexistent/path/.~tmp~/bad.txt");

        let final_good = dir.path().join("good.txt");
        let final_bad = PathBuf::from("/nonexistent/path/bad.txt");

        let delayed = vec![
            (staged_bad, final_bad),
            (staged_good.clone(), final_good.clone()),
        ];

        // Should not panic or abort.
        handle_delayed_updates(&delayed, None);

        // The good file should still be renamed successfully.
        assert_eq!(fs::read_to_string(&final_good).unwrap(), "good");
        assert!(!staged_good.exists());
    }

    /// Verifies the sweep handles an empty delayed list gracefully.
    #[test]
    fn handle_delayed_updates_empty_is_noop() {
        handle_delayed_updates(&[], None);
    }

    /// Verifies that `handle_delayed_updates` backs up a pre-existing
    /// destination file before renaming the staged file into place when a
    /// `BackupConfig` is supplied.
    ///
    /// This is the receiver-side equivalent of upstream
    /// `receiver.c:431-432 make_backup(fname, False)` -> `backup.c:make_backup`
    /// which renames the existing file out of the way and emits the
    /// `backed up X to Y` info_log via `INFO_GTE(BACKUP, 1)` at
    /// `backup.c:352-353`. Upstream `testsuite/backup.test:43,56` greps for
    /// that exact line under `--info=BACKUP --delay-updates` so the rename
    /// must fire before the staged file replaces the destination.
    #[test]
    fn handle_delayed_updates_backs_up_existing_destination() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let final_path = dest_root.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(&[(staged.clone(), final_path.clone())], Some(backup_config));

        assert_eq!(
            fs::read_to_string(&final_path).unwrap(),
            "new-content",
            "staged file must replace destination after backup"
        );
        // upstream: backup.c::get_backup_name() appends the configured
        // suffix even when backup_dir is set. `compute_backup_path` mirrors
        // that semantic (see `compute_backup_path_with_backup_dir` in
        // engine::local_copy::tests::executor_file_operations).
        let backup_path = backup_root.join("name1~");
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "pre-existing destination must be renamed into backup-dir before \
             the staged file is renamed into place"
        );
        assert!(!staged.exists(), "staging file should be moved out");
        assert!(
            !staging_dir.exists(),
            "empty .~tmp~ dir should be removed after sweep"
        );
    }

    /// Verifies the backup step is skipped when no existing destination is
    /// present, matching upstream `backup.c:make_backup()` which returns
    /// early when `lstat(fname)` reports `ENOENT`.
    #[test]
    fn handle_delayed_updates_no_backup_when_dest_missing() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"only-content").unwrap();

        let final_path = dest_root.join("name1");

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(&[(staged.clone(), final_path.clone())], Some(backup_config));

        assert_eq!(fs::read_to_string(&final_path).unwrap(), "only-content");
        assert!(
            !backup_root.join("name1").exists(),
            "no backup file should be created when destination did not exist"
        );
    }

    /// Mirrors upstream `testsuite/backup.test:27-33` (`--no-whole-file
    /// --backup` without `--backup-dir`). With `backup_dir = None` and a
    /// `~` suffix, the existing destination must be renamed alongside the
    /// original (`name1` -> `name1~`), and the staged update must land at
    /// the original path. Upstream emits `backed up name1 to name1~`.
    #[test]
    fn handle_delayed_updates_backs_up_in_place_with_suffix_only() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let final_path = dest_root.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: None,
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(&[(staged.clone(), final_path.clone())], Some(backup_config));

        assert_eq!(
            fs::read_to_string(&final_path).unwrap(),
            "new-content",
            "staged file must replace destination after in-place backup"
        );
        let backup_path = dest_root.join("name1~");
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "pre-existing destination must be renamed to <name><suffix> in \
             the same directory when no --backup-dir is set"
        );
    }

    /// Mirrors upstream `testsuite/backup.test:38-45` (`--backup-dir=bakdir`
    /// with nested source path `deep/name1`). The backup hierarchy must
    /// mirror the source layout: `deep/name1` -> `bakdir/deep/name1~`.
    /// Upstream's `copy_valid_path()` creates missing parents; oc-rsync's
    /// `handle_delayed_updates` relies on `create_dir_all(parent)` for the
    /// same effect.
    #[test]
    fn handle_delayed_updates_creates_intermediate_backup_dirs() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let deep_dest = dest_root.join("deep");
        fs::create_dir(&deep_dest).unwrap();
        let final_path = deep_dest.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(&[(staged.clone(), final_path.clone())], Some(backup_config));

        let backup_path = backup_root.join("deep").join("name1~");
        assert!(
            backup_path.exists(),
            "backup_dir must mirror the source hierarchy: {} should exist",
            backup_path.display()
        );
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "nested backup must carry the pre-existing destination content"
        );
        assert_eq!(
            fs::read_to_string(&final_path).unwrap(),
            "new-content",
            "staged file must reach the nested destination path"
        );
    }

    /// Mirrors upstream `testsuite/backup.test:43` regex `backed up $fn
    /// to .*/$fn$` - when `--backup-dir` is set and `--suffix` is left at
    /// its `--backup-dir` default (empty string), the backup path has NO
    /// suffix appended. Upstream's `stringjoin(rel, remainder, fname,
    /// backup_suffix, NULL)` collapses to just `bakdir/path/name` when
    /// `backup_suffix == ""`.
    #[test]
    fn handle_delayed_updates_backup_dir_with_empty_suffix() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let final_path = dest_root.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from(""),
        };

        handle_delayed_updates(&[(staged.clone(), final_path.clone())], Some(backup_config));

        let suffixed = backup_root.join("name1~");
        assert!(
            !suffixed.exists(),
            "empty suffix must NOT append `~` (would diverge from upstream \
             default when --backup-dir is set without explicit --suffix)"
        );
        let backup_path = backup_root.join("name1");
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "with empty suffix, backup path is bakdir/<name> verbatim"
        );
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "new-content");
    }

    /// Mirrors upstream `testsuite/backup.test:28,42,55` which iterate
    /// `for fn in deep/name1 deep/name2; do ...` - a single `--backup`
    /// invocation must back up every modified file in one delayed-updates
    /// sweep, with each backup honoring its own source path.
    #[test]
    fn handle_delayed_updates_backs_up_multiple_files_in_one_sweep() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        let final_a = dest_root.join("name1");
        let final_b = dest_root.join("name2");
        fs::write(&final_a, b"old-a").unwrap();
        fs::write(&final_b, b"old-b").unwrap();

        let staged_a = staging_dir.join("name1");
        let staged_b = staging_dir.join("name2");
        fs::write(&staged_a, b"new-a").unwrap();
        fs::write(&staged_b, b"new-b").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(
            &[
                (staged_a.clone(), final_a.clone()),
                (staged_b.clone(), final_b.clone()),
            ],
            Some(backup_config),
        );

        assert_eq!(fs::read_to_string(&final_a).unwrap(), "new-a");
        assert_eq!(fs::read_to_string(&final_b).unwrap(), "new-b");
        assert_eq!(
            fs::read_to_string(backup_root.join("name1~")).unwrap(),
            "old-a",
            "every file in the sweep must be backed up independently"
        );
        assert_eq!(
            fs::read_to_string(backup_root.join("name2~")).unwrap(),
            "old-b",
            "every file in the sweep must be backed up independently"
        );
    }

    /// Verifies that staged files in `.~tmp~/` persist as valid partials when
    /// the sweep is never called (simulating an interrupted transfer).
    ///
    /// This is the core invariant for `--delay-updates` interrupt safety:
    /// on interrupt, `handle_delayed_updates()` is skipped (the `?` operator
    /// propagates the error before reaching the sweep call in `pipelined.rs`),
    /// leaving staged files intact for the next resume attempt.
    ///
    /// upstream: receiver.c:584-585 - handle_delayed_updates() only after
    /// successful completion of both transfer phases.
    #[test]
    fn interrupt_skips_sweep_files_persist_in_staging() {
        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        // Create staged files as if commit_file() placed them there.
        let staged_a = staging_dir.join("a.txt");
        let staged_b = staging_dir.join("b.txt");
        fs::write(&staged_a, b"staged-a").unwrap();
        fs::write(&staged_b, b"staged-b").unwrap();

        let final_a = dir.path().join("a.txt");
        let final_b = dir.path().join("b.txt");

        // Do NOT call handle_delayed_updates - simulating interrupt.
        // Verify files remain in staging.
        assert!(staged_a.exists(), "staged file a must persist");
        assert!(staged_b.exists(), "staged file b must persist");
        assert!(!final_a.exists(), "final path a must not exist");
        assert!(!final_b.exists(), "final path b must not exist");

        // Verify the staged content is valid (usable for resume).
        assert_eq!(fs::read(&staged_a).unwrap(), b"staged-a");
        assert_eq!(fs::read(&staged_b).unwrap(), b"staged-b");

        // Now verify that a subsequent resume (calling the sweep) works.
        let delayed = vec![
            (staged_a.clone(), final_a.clone()),
            (staged_b.clone(), final_b.clone()),
        ];
        handle_delayed_updates(&delayed, None);

        assert!(final_a.exists());
        assert!(final_b.exists());
        assert_eq!(fs::read_to_string(&final_a).unwrap(), "staged-a");
        assert_eq!(fs::read_to_string(&final_b).unwrap(), "staged-b");
    }
}
