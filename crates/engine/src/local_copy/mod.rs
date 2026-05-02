//! # Overview
//!
//! Implements deterministic local filesystem copies used by the current
//! `rsync` development snapshot. The module constructs
//! [`LocalCopyPlan`] values from CLI-style operands and executes them while
//! preserving permissions, timestamps, and optional ownership metadata via
//! [`metadata`].
//!
//! # Design
//!
//! - [`LocalCopyPlan`] encapsulates parsed operands and exposes
//!   [`LocalCopyPlan::execute`] for performing the copy.
//! - [`LocalCopyError`] mirrors upstream exit codes so higher layers can render
//!   canonical diagnostics.
//! - [`LocalCopyOptions`] configures behaviours such as deleting destination
//!   entries that are absent from the source when `--delete` is requested,
//!   pruning excluded entries when `--delete-excluded` is enabled, or
//!   preserving ownership/group metadata when `--owner`/`--group` are supplied.
//! - Helper functions preserve metadata after content writes, matching upstream
//!   rsync's ordering and covering regular files, directories, symbolic links,
//!   FIFOs, and device nodes when the caller enables the corresponding options.
//!   Hard linked files are reproduced as hard links in the destination when the
//!   platform exposes inode identifiers, and optional sparse handling skips
//!   zero-filled regions when requested so destination files retain holes present
//!   in the source.
//!
//! # Invariants
//!
//! - Plans never mutate their source list after construction.
//! - Copy operations create parent directories before writing files or links.
//! - Metadata application occurs after file contents are written.
//!
//! # Examples
//!
//! ```
//! use engine::local_copy::LocalCopyPlan;
//! use std::ffi::OsString;
//!
//! # let temp = tempfile::tempdir().unwrap();
//! # let source = temp.path().join("source.txt");
//! # let dest = temp.path().join("dest.txt");
//! # std::fs::write(&source, b"data").unwrap();
//! # std::fs::write(&dest, b"").unwrap();
//! let operands = vec![OsString::from("source.txt"), OsString::from("dest.txt")];
//! let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
//! # let operands = vec![source.into_os_string(), dest.into_os_string()];
//! # let plan = LocalCopyPlan::from_operands(&operands).unwrap();
//! let summary = plan.execute().expect("copy succeeds");
//! assert_eq!(summary.files_copied(), 1);
//! ```

/// Buffer pool for I/O buffer reuse during large file transfers.
pub mod buffer_pool;
pub mod clonefile;
mod compressor;
mod context;
#[cfg(test)]
mod debug_del;
#[cfg(test)]
mod debug_deltasum;
#[cfg(test)]
mod debug_flist;
#[cfg(test)]
mod debug_recv;
#[cfg(test)]
mod debug_send;
mod deferred_sync;
/// Deletion logic for rsync `--delete` variants.
///
/// Provides helpers for `--delete-before`, `--delete-during`, `--delete-after`,
/// and `--delete-delay` timing modes including `DeletionContext`,
/// `should_delete_entry`, and `build_keep_set`.
pub mod deletion;
mod dir_merge;
mod error;
mod executor;
mod filter_program;
mod hard_links;
mod metadata_sync;
mod operands;
mod options;
mod overrides;
/// State machine for 4-priority streaming file list processing pipeline.
///
/// Provides the [`PipelineController`](pipelined_state::PipelineController) for
/// coordinating concurrent file list reception, pipeline filling, entry processing,
/// and response handling with a priority-driven main loop.
pub mod pipelined_state;
mod plan;
pub(crate) mod prefetch;
mod skip_compress;
pub mod win_copy;

pub use buffer_pool::{
    BorrowedBufferGuard, BufferAllocator, BufferGuard, BufferPool, BufferPoolStats,
    DefaultAllocator, GlobalBufferPoolConfig, ThroughputTracker, global_buffer_pool,
    init_global_buffer_pool,
};
pub use deferred_sync::{DeferredSync, SyncStrategy};

pub use plan::{
    LocalCopyAction, LocalCopyChangeSet, LocalCopyExecution, LocalCopyFileKind, LocalCopyMetadata,
    LocalCopyPlan, LocalCopyProgress, LocalCopyRecord, LocalCopyRecordHandler, LocalCopyReport,
    LocalCopySummary, TimeChange,
};

pub use options::{
    BuilderError, DeleteTiming, LocalCopyOptions, LocalCopyOptionsBuilder, ReferenceDirectory,
    ReferenceDirectoryKind,
};

pub use error::{LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind};

#[cfg(test)]
pub(crate) use plan::FilterOutcome;

pub use skip_compress::{SkipCompressList, SkipCompressParseError};

pub(crate) use compressor::ActiveCompressor;
pub(crate) use context::{
    CopyContext, CopyOutcome, CreatedEntryKind, DeferredUpdate, FinalizeMetadataParams,
    MetadataPathContext, OwnedPathContext,
};

#[allow(unused_imports)] // REASON: convenience re-export; not all items used in every module
pub(crate) use dir_merge::{
    FilterParseError, ParsedFilterDirective, apply_dir_merge_rule_defaults,
    filter_program_local_error, load_dir_merge_rules_recursive, parse_filter_directive_line,
    resolve_dir_merge_path,
};

pub(crate) use executor::*;
pub use executor::{
    DestinationWriteGuard, PartialFileManager, PartialMode, SparseDetector, SparseReader,
    SparseRegion, SparseWriter, compute_backup_path, remove_existing_destination,
    remove_incomplete_destination,
};

pub(crate) use hard_links::HardLinkTracker;
pub use hard_links::{HardlinkApplyResult, HardlinkApplyTracker};

pub(crate) use metadata_sync::map_metadata_error;

#[cfg(all(any(unix, windows), feature = "acl"))]
pub(crate) use metadata_sync::sync_acls_if_requested;

#[cfg(all(unix, feature = "xattr"))]
pub(crate) use metadata_sync::sync_xattrs_if_requested;

#[cfg(all(unix, feature = "xattr"))]
pub(crate) use metadata_sync::sync_nfsv4_acls_if_requested;

pub(crate) use operands::{DestinationSpec, SourceSpec, operand_is_remote};

pub use filter_program::{
    DirMergeEnforcedKind, DirMergeOptions, DirMergeRule, ExcludeIfPresentRule, FilterProgram,
    FilterProgramEntry, FilterProgramError,
};

#[cfg(test)]
pub(crate) use filter_program::{FilterContext, FilterSegment};

use std::sync::atomic::AtomicUsize;

const COPY_BUFFER_SIZE: usize = 128 * 1024;

/// Buffer size for files smaller than 64 KB (8 KB).
const ADAPTIVE_BUFFER_TINY: usize = 8 * 1024;
/// Buffer size for files in the 64 KB .. 1 MB range (32 KB).
const ADAPTIVE_BUFFER_SMALL: usize = 32 * 1024;
/// Buffer size for files in the 1 MB .. 64 MB range (128 KB).
const ADAPTIVE_BUFFER_MEDIUM: usize = 128 * 1024;
/// Buffer size for files in the 64 MB .. 256 MB range (512 KB).
const ADAPTIVE_BUFFER_LARGE: usize = 512 * 1024;
/// Buffer size for files 256 MB and larger (1 MB).
///
/// Reduces syscall count on the read/write fallback path when
/// `copy_file_range` is unavailable. For a 1 GB file this means 1024
/// read+write pairs instead of 2048 with a 512 KB buffer.
const ADAPTIVE_BUFFER_HUGE: usize = 1024 * 1024;

/// File-size threshold below which [`ADAPTIVE_BUFFER_TINY`] is used (64 KB).
const ADAPTIVE_THRESHOLD_TINY: u64 = 64 * 1024;
/// File-size threshold below which [`ADAPTIVE_BUFFER_SMALL`] is used (1 MB).
const ADAPTIVE_THRESHOLD_SMALL: u64 = 1024 * 1024;
/// File-size threshold below which [`ADAPTIVE_BUFFER_MEDIUM`] is used (64 MB).
const ADAPTIVE_THRESHOLD_MEDIUM: u64 = 64 * 1024 * 1024;
/// File-size threshold below which [`ADAPTIVE_BUFFER_LARGE`] is used (256 MB).
const ADAPTIVE_THRESHOLD_LARGE: u64 = 256 * 1024 * 1024;

/// Selects an I/O buffer size appropriate for the given file size.
///
/// The returned size balances memory consumption against throughput:
///
/// | File size          | Buffer size |
/// |--------------------|-------------|
/// | < 64 KB            | 8 KB        |
/// | 64 KB .. < 1 MB    | 32 KB       |
/// | 1 MB .. < 64 MB    | 128 KB      |
/// | 64 MB .. < 256 MB  | 512 KB      |
/// | >= 256 MB          | 1 MB        |
#[must_use]
pub(crate) const fn adaptive_buffer_size(file_size: u64) -> usize {
    if file_size < ADAPTIVE_THRESHOLD_TINY {
        ADAPTIVE_BUFFER_TINY
    } else if file_size < ADAPTIVE_THRESHOLD_SMALL {
        ADAPTIVE_BUFFER_SMALL
    } else if file_size < ADAPTIVE_THRESHOLD_MEDIUM {
        ADAPTIVE_BUFFER_MEDIUM
    } else if file_size < ADAPTIVE_THRESHOLD_LARGE {
        ADAPTIVE_BUFFER_LARGE
    } else {
        ADAPTIVE_BUFFER_HUGE
    }
}

static NEXT_TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
const CROSS_DEVICE_ERROR_CODE: i32 = 18;

#[cfg(windows)]
const CROSS_DEVICE_ERROR_CODE: i32 = 17;

#[cfg(not(any(unix, windows)))]
const CROSS_DEVICE_ERROR_CODE: i32 = 18;

#[cfg(test)]
pub(crate) fn with_hard_link_override<F, R>(override_fn: F, action: impl FnOnce() -> R) -> R
where
    F: Fn(&std::path::Path, &std::path::Path) -> std::io::Result<()> + 'static,
{
    overrides::with_hard_link_override(override_fn, action)
}

#[cfg(test)]
pub(crate) fn with_device_id_override<F, R>(override_fn: F, action: impl FnOnce() -> R) -> R
where
    F: Fn(&std::path::Path, &std::fs::Metadata) -> Option<u64> + 'static,
{
    overrides::with_device_id_override(override_fn, action)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "filter_program_internal_tests.rs"]
mod filter_program_internal_tests;

#[cfg(test)]
pub(crate) mod test_support {
    #[allow(unused_imports)]
    pub(crate) use super::executor::take_fsync_call_count;
}
