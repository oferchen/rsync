//! File processing logic for the disk commit thread.
//!
//! Handles chunked file writes, whole-file coalesced writes, output file
//! opening (device, inplace, temp+rename), and metadata application.
//!
//! Metadata is applied to the temp file before rename to match upstream
//! `rsync.c:finish_transfer()` line 748: "Change permissions before putting
//! the file into place." When rename crosses device boundaries (EXDEV), a
//! copy+remove fallback re-applies metadata to the final path since
//! `fs::copy` does not preserve ownership, timestamps, ACLs, or xattrs.

/// Commit path: backup, atomic rename, inplace truncation, cross-device
/// fallback, and partial-file retention.
mod commit;
/// `--delay-updates` staging and the bulk rename sweep.
mod delayed;
/// Per-file processing: chunked writes, whole-file writes, output file
/// opening, and writer backend selection.
mod file_ops;
/// Post-commit metadata, ACL, and xattr application.
mod metadata;

#[cfg(test)]
mod tests;

pub use self::delayed::{DelayedUpdateEntry, delay_updates_staging_path, handle_delayed_updates};
pub(super) use self::file_ops::{process_file, process_whole_file};

// Re-exported for the in-module test suite (`use super::*`), which exercises
// the rename/cross-device/backup helpers and the writer selector directly.
#[cfg(test)]
use self::commit::{is_cross_device, make_backup, partial_dir_path, rename_with_io_uring_fallback};
#[cfg(test)]
use self::file_ops::make_writer;
#[cfg(test)]
use super::config::BackupConfig;
#[cfg(test)]
use super::writer::Writer;
