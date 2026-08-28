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
/// Per-file processing: chunked writes, whole-file writes, output file
/// opening, and writer backend selection.
mod file_ops;
/// Post-commit metadata, ACL, and xattr application.
mod metadata;

#[cfg(test)]
mod tests;

pub(crate) use self::commit::make_backup;
pub(super) use self::file_ops::{process_file, process_whole_file};

/// Forces the backup ladder's link and rename tiers onto `EXDEV` for the
/// guard's lifetime, so a test can exercise the cross-device `copy_file()`
/// tier (upstream `backup.c:414` `make_backup: COPY`) without a second
/// filesystem. Shared with the `--delay-updates` sweep's tests, which reach
/// the same ladder.
#[cfg(test)]
pub(crate) use self::commit::ForceExdev;
#[cfg(all(test, unix))]
use self::commit::rename_config_sandboxed;
#[cfg(test)]
use self::commit::{
    delay_updates_staging_path, is_cross_device, make_backup_copy, rename_with_io_uring_fallback,
};
#[cfg(all(test, target_os = "macos"))]
use self::file_ops::make_writer;
#[cfg(test)]
use self::file_ops::truncate_for_whole_file_sparse;
#[cfg(test)]
use super::config::{BackupConfig, DiskCommitConfig};
#[cfg(all(test, target_os = "macos"))]
use super::writer::Writer;
#[cfg(test)]
use crate::pipeline::messages::BeginMessage;
