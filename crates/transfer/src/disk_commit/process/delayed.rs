//! `--delay-updates` staging and the bulk rename sweep.
//!
//! Files staged to `.~tmp~/<filename>` during commit are renamed to their
//! final destinations after all files are committed, mirroring upstream
//! `receiver.c:529-557` `handle_delayed_updates()`.

use std::io;
use std::path::{Path, PathBuf};

use super::commit::rename_with_io_uring_fallback;

/// Entry for the `--delay-updates` bulk rename sweep.
///
/// Each entry records the staging path (inside `.~tmp~/`) and the final
/// destination path. After all files are committed by the disk thread, the
/// caller collects these entries and passes them to
/// [`handle_delayed_updates`] for the bulk rename sweep.
#[derive(Debug, Clone)]
pub struct DelayedUpdateEntry {
    /// Path where the file was staged (inside the `.~tmp~/` directory).
    pub staging_path: PathBuf,
    /// Final destination path where the file should appear.
    pub final_path: PathBuf,
}

/// Performs the `--delay-updates` bulk rename sweep.
///
/// After all files have been committed to their staging paths (inside
/// `.~tmp~/` subdirectories), this function renames each staged file to its
/// final destination and removes the now-empty staging directories.
///
/// Mirrors upstream `receiver.c:529-557` which iterates the list of delayed
/// files, calls `handle_partial_dir(partialptr, PDIR_DELETE)` to rename each
/// file from the partial directory to its final path, then removes the
/// partial directory itself.
///
/// # Errors
///
/// Returns the first rename error encountered. Files that have already been
/// renamed are not rolled back - the operation is best-effort, matching
/// upstream rsync behavior where a failed rename is logged and the sweep
/// continues.
pub fn handle_delayed_updates(outcomes: &[DelayedUpdateEntry]) -> io::Result<()> {
    // upstream: receiver.c:529-557 - iterate delayed_bits, rename each file
    // from partialptr to fname, then handle_partial_dir(partialptr, PDIR_DELETE)
    let mut staging_dirs = std::collections::BTreeSet::new();

    for outcome in outcomes {
        // Track staging directories for cleanup.
        if let Some(parent) = outcome.staging_path.parent() {
            if parent
                .file_name()
                .is_some_and(|name| name == super::super::config::DELAY_UPDATES_PARTIAL_DIR)
            {
                staging_dirs.insert(parent.to_path_buf());
            }
        }

        // upstream: receiver.c:542 - do_rename(partialptr, fname)
        if let Some(parent) = outcome.final_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        rename_with_io_uring_fallback(&outcome.staging_path, &outcome.final_path)?;

        logging::debug_log!(
            Io,
            1,
            "delay-updates: renamed {} -> {}",
            outcome.staging_path.display(),
            outcome.final_path.display()
        );
    }

    // upstream: receiver.c:554 - handle_partial_dir(partialptr, PDIR_DELETE)
    // Remove empty staging directories after all renames complete.
    for dir in staging_dirs.iter().rev() {
        let _ = std::fs::remove_dir(dir);
    }

    Ok(())
}

/// Computes the staging path for a file under `--delay-updates`.
///
/// Given a file's final destination path, returns the path where it should be
/// staged inside the `.~tmp~/` subdirectory of the file's parent directory.
///
/// # Example
///
/// ```text
/// final_path:   /dest/subdir/file.txt
/// staging_path: /dest/subdir/.~tmp~/file.txt
/// ```
///
/// # Upstream Reference
///
/// - `receiver.c:820` - compute `partialptr` from `partial_dir` + `fname`
pub fn delay_updates_staging_path(final_path: &Path) -> PathBuf {
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = final_path.file_name().unwrap_or(final_path.as_os_str());
    parent
        .join(super::super::config::DELAY_UPDATES_PARTIAL_DIR)
        .join(file_name)
}
