//! Batch replay logic for applying recorded delta operations to a destination.
//!
//! This module contains the core replay implementation that reads a batch file
//! and applies the recorded delta operations to reconstruct files at the
//! destination. The replay logic is decoupled from the orchestration layer
//! (core crate) so it can be tested and reused independently.
//!
//! # Overview
//!
//! Replay proceeds in three phases:
//!
//! 1. **Header validation**: The batch header is read and the stream flags
//!    bitmap is verified against the protocol version.
//! 2. **File list decoding**: The protocol flist wire format is decoded using
//!    [`protocol::flist::FileListReader`], matching the encoding produced by
//!    [`protocol::flist::FileListWriter`] during batch write.
//! 3. **Directory and metadata application**: Parent directories are created,
//!    symlinks are materialized, and metadata (permissions, timestamps) is
//!    applied to all entries.
//!
//! Delta replay for regular files is a separate concern - the batch body
//! after the flist contains delta operations that reference basis files at
//! the destination.
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
use fs_ops::{apply_entry_metadata, create_symlink};

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
                if dest_path.symlink_metadata().is_ok() {
                    let _ = apply_entry_metadata(&dest_path, entry, flags);
                }
            }
            _ => {}
        }
    }
}
