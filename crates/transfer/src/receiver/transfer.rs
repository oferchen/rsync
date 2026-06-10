//! Transfer orchestration for the receiver role.
//!
//! Provides the `run`, `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! entry points plus the common `setup_transfer` initialization. The driving
//! loops live in their own submodules:
//!
//! - `sync` - sequential per-file transfer used by `run_sync`.
//! - `pipelined` - decoupled two-phase pipeline used by `run_pipelined`.
//! - `pipelined_incremental` - same as `pipelined` plus incremental directory
//!   creation and failed-dir tracking.
//! - `setup` - common multiplex/filter/file-list setup.
//! - `phases` - protocol phase exchange and goodbye handshake.
//! - `candidates` - candidate-file selection for the pipelined paths.
//! - `pipeline` - the inner `run_pipeline_loop_decoupled` plus dry-run loop.

mod candidates;
mod phases;
mod pipeline;
mod pipelined;
mod pipelined_incremental;
mod setup;
mod sync;

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::debug_log;

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
                if let Err(e) = fs::rename(final_path, &backup_path) {
                    eprintln!("rsync: backup failed for {}: {e}", final_path.display());
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

    // --- PIR-5.d: Interrupt behavior ---

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
