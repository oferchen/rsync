//! Batch replay logic for applying recorded delta operations to a destination.
//!
//! This module contains the core replay implementation that reads a batch file
//! and applies the recorded delta operations to reconstruct files at the
//! destination. The replay logic is decoupled from the orchestration layer
//! (core crate) so it can be tested and reused independently.
//!
//! # Overview
//!
//! Replay first reads the batch header and reconciles its stream flags against
//! the active options, then decodes the file list from the protocol flist wire
//! format (matching the encoding produced during batch write). It then applies
//! the entries in three phases:
//!
//! 1. **Directories and symlinks**: Directories are created and symlinks are
//!    materialized, and parent directories are ensured for regular files.
//! 2. **Delta application**: Per-file delta operations from the batch body are
//!    applied against the basis files at the destination.
//! 3. **Metadata**: Permissions, timestamps, and ownership are applied to all
//!    entries, with directories updated last so earlier file writes do not
//!    disturb their directory timestamps.
//!
//! # Submodules
//!
//! - `codec` - compression codec detection (`zlib` vs `zstd`) and decoder
//!   construction.
//! - `delta` - low-level delta-application primitives (`apply_delta_ops`,
//!   `write_literals_to_file`, `choose_block_length`).
//! - `dispatch` - per-file helpers used by the main loop: iflags decoding,
//!   sum-head reading, compressed-token streaming, temp-file commit.
//! - `delta_phase` - the NDX-stream loop that drives per-file delta application.
//! - `fs_ops` - symlink creation and metadata application primitives.
//!
//! # Upstream Reference
//!
//! - `batch.c:read_stream_flags()` - reads the stream flags bitmap
//! - `main.c:do_recv()` - orchestrates file list + delta application
//! - `receiver.c:recv_files()` - per-file delta application

mod codec;
mod delta;
mod delta_phase;
mod dispatch;
mod fs_ops;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;

use protocol::flist::sort_file_list;

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::format::BatchFlags;
use crate::reader::BatchReader;

use delta_phase::apply_delta_phase;
use fs_ops::{apply_entry_metadata, apply_symlink_entry_metadata, create_symlink};

pub use delta::apply_delta_ops;

/// Result of a batch replay operation.
///
/// Contains aggregate statistics about the files processed during replay.
/// The caller can use these to report progress or build higher-level
/// summary types.
#[derive(Debug, Clone, Default)]
pub struct ReplayResult {
    /// Number of files processed during replay.
    pub file_count: u64,
    /// Total size in bytes of all processed files.
    pub total_size: u64,
    /// Whether the batch header had the recurse flag set.
    pub recurse: bool,
    /// Number of directories created during replay.
    pub dirs_created: u64,
    /// Number of symlinks created during replay.
    pub symlinks_created: u64,
}

/// Replay a batch file, applying recorded delta operations to a destination.
///
/// Opens the batch file described by `batch_cfg`, reads its header and
/// decodes the file list using the protocol flist wire format. For each
/// entry, the appropriate filesystem object is created (directory, symlink,
/// or regular file) and metadata (permissions, timestamps, ownership) is
/// applied.
///
/// Regular file delta replay reads delta operations from the batch body
/// after the file list and applies them against the existing basis file
/// at the destination path.
///
/// # Arguments
///
/// * `batch_cfg` - Configuration identifying the batch file to replay.
/// * `dest_root` - Root directory where files are reconstructed.
/// * `verbosity` - Verbosity level controlling stdout output (0 = silent).
///
/// # Returns
///
/// A [`ReplayResult`] with aggregate statistics about the replay.
///
/// # Errors
///
/// Returns [`BatchError`] if the batch file cannot be opened, the header
/// is invalid, file entries cannot be decoded, or delta application fails.
pub fn replay(
    batch_cfg: &BatchConfig,
    dest_root: &Path,
    verbosity: i32,
) -> BatchResult<ReplayResult> {
    let mut reader = BatchReader::new((*batch_cfg).clone())?;

    let flags = reader.read_header()?;

    // upstream: batch.c:120 check_batch_flags() - reconcile the active options
    // against the batch's recorded stream flags. Non-iconv mismatches are
    // forced to match the batch and mentioned at --info=misc (verbose >= 1);
    // an --iconv mismatch is fatal.
    for message in crate::format::check_batch_flags(
        flags,
        batch_cfg.active_flags,
        reader.config().protocol_version,
    )? {
        if verbosity >= 1 {
            println!("{message}");
        }
    }

    let mut entries = reader.read_protocol_flist()?;

    // upstream: flist.c:2736 - flist_sort_and_clean() after recv_file_list().
    // NDX values from the generator reference sorted positions, not wire order.
    let pre29 = reader.config().protocol_version < 29;
    sort_file_list(&mut entries, false, pre29);

    let mut result = ReplayResult {
        file_count: entries.len() as u64,
        recurse: flags.recurse,
        ..ReplayResult::default()
    };

    // upstream: main.c:778-799 - get_local_name() creates the destination
    // directory automatically when the transfer involves more than one file or
    // the destination operand ends in a slash. Batch replay reproduces that
    // behaviour so a fresh destination tree can absorb a batch without
    // requiring the caller to pre-create the root. The flist root entry "."
    // joined to a missing dest_root expands to `dest_root/.`, which
    // `fs::create_dir_all` cannot materialise because the parent does not
    // exist; creating dest_root first sidesteps that path-component edge.
    ensure_dest_root(dest_root, &entries, &mut result)?;

    // Phase 1: Create directories and symlinks, ensure parent dirs for regular files.
    prepare_directories_and_symlinks(&entries, dest_root, verbosity, &mut result)?;

    // Phase 2: Apply delta operations for regular files.
    apply_delta_phase(
        &mut reader,
        &mut entries,
        dest_root,
        &flags,
        &mut result,
        verbosity,
    )?;

    // Phase 3: Apply metadata. Directories are done last because setting
    // timestamps on a directory before writing its contents would cause the
    // mtime to be updated by the file writes. Regular files and symlinks get
    // metadata immediately.
    apply_all_metadata(&entries, dest_root, &flags, verbosity);

    Ok(result)
}

/// Phase 1: Create directories and symlinks, ensure parent dirs for regular files.
///
/// Directories must be created before files so that parent paths exist.
fn prepare_directories_and_symlinks(
    entries: &[protocol::flist::FileEntry],
    dest_root: &Path,
    verbosity: i32,
    result: &mut ReplayResult,
) -> BatchResult<()> {
    for entry in entries {
        let dest_path = dest_root.join(entry.name());
        result.total_size += entry.size();

        if verbosity > 0 {
            println!("{}", entry.name());
        }

        match entry.file_type() {
            protocol::flist::FileType::Directory => {
                if !dest_path.exists() {
                    fs::create_dir_all(&dest_path).map_err(|e| {
                        BatchError::Io(std::io::Error::new(
                            e.kind(),
                            format!("failed to create directory '{}': {e}", dest_path.display()),
                        ))
                    })?;
                    result.dirs_created += 1;
                }
            }
            protocol::flist::FileType::Symlink => {
                if let Some(target) = entry.link_target() {
                    ensure_parent_dir(&dest_path)?;
                    create_symlink(target, &dest_path)?;
                    result.symlinks_created += 1;
                }
            }
            protocol::flist::FileType::Regular => {
                ensure_parent_dir(&dest_path)?;
            }
            // Block devices, char devices, FIFOs, sockets - skip during
            // batch replay (upstream rsync also skips special files in
            // batch mode unless running as root)
            _ => {}
        }
    }
    Ok(())
}

/// Ensure the destination root exists before per-entry processing.
///
/// Mirrors upstream `get_local_name()` semantics: a missing destination
/// directory is materialised automatically when the batch carries more than
/// one entry or any directory entry, so callers can replay into a fresh path
/// without a separate `mkdir`. The dirs_created counter is bumped so the
/// summary reflects the same "created directory" notice upstream emits.
fn ensure_dest_root(
    dest_root: &Path,
    entries: &[protocol::flist::FileEntry],
    result: &mut ReplayResult,
) -> BatchResult<()> {
    if dest_root.as_os_str().is_empty() || dest_root.exists() {
        return Ok(());
    }
    let needs_dir = entries.len() > 1
        || entries
            .iter()
            .any(|entry| entry.file_type() == protocol::flist::FileType::Directory);
    if !needs_dir {
        return Ok(());
    }
    fs::create_dir_all(dest_root).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to create destination directory '{}': {e}",
                dest_root.display()
            ),
        ))
    })?;
    result.dirs_created += 1;
    Ok(())
}

/// Create the parent directory of `path` if it does not already exist.
fn ensure_parent_dir(path: &Path) -> BatchResult<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "failed to create parent directory '{}': {e}",
                        parent.display()
                    ),
                ))
            })?;
        }
    }
    Ok(())
}

/// Phase 3: Apply metadata to every entry.
///
/// Directories and regular files use [`apply_entry_metadata`]; failures are
/// reported via the verbosity channel but never abort replay (rsync's batch
/// mode treats metadata application as best-effort - typical when running
/// as a non-root user trying to set ownership).
///
/// Symlink metadata is applied via lchown/lutimes on platforms that support
/// it. The `metadata` crate handles this transparently.
fn apply_all_metadata(
    entries: &[protocol::flist::FileEntry],
    dest_root: &Path,
    flags: &BatchFlags,
    verbosity: i32,
) {
    for entry in entries {
        let dest_path = dest_root.join(entry.name());

        match entry.file_type() {
            protocol::flist::FileType::Directory | protocol::flist::FileType::Regular => {
                if dest_path.exists() {
                    if let Err(e) = apply_entry_metadata(&dest_path, entry, flags) {
                        if verbosity > 0 {
                            println!(
                                "  warning: could not apply metadata to '{}': {e}",
                                dest_path.display()
                            );
                        }
                    }
                }
            }
            protocol::flist::FileType::Symlink => {
                // upstream: rsync.c:set_file_attrs() - chmod is skipped for
                // symlinks because most platforms ignore the mode bits and
                // chmod through a symlink follows the link. Calling
                // apply_entry_metadata here clobbers the target file's mode
                // with the symlink entry's mode (typically 0o777), which
                // breaks the batch-mode upstream testsuite case
                // `nolf-symlink -> nolf` (nolf ends up as 0o777 instead of
                // the recorded 0o644).
                if dest_path.symlink_metadata().is_ok() {
                    let _ = apply_symlink_entry_metadata(&dest_path, entry, flags);
                }
            }
            _ => {}
        }
    }
}
