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
/// Concurrent delta generation infrastructure using rayon.
///
/// See [`concurrent_delta::DeltaPipeline`] for the main entry point.
pub mod concurrent_delta;
mod context;
pub(crate) mod debug_flist;
mod deferred_sync;
pub(crate) mod parallel_transfer;
pub(crate) mod prefetch;
pub mod win_copy;
/// Deletion strategy implementations for rsync `--delete` variants.
///
/// Provides [`DeletionStrategy`](deletion::DeletionStrategy) implementations
/// for `--delete-before`, `--delete-during`, `--delete-after`, and
/// `--delete-delay` timing modes. Use [`apply_deletion_strategy`](deletion::apply_deletion_strategy)
/// to obtain the appropriate strategy for a given [`DeleteTiming`].
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
mod plan;
mod skip_compress;

pub use buffer_pool::{BufferGuard, BufferPool};
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
};

#[allow(unused_imports)]
pub(crate) use dir_merge::{
    FilterParseError, ParsedFilterDirective, apply_dir_merge_rule_defaults,
    filter_program_local_error, load_dir_merge_rules_recursive, parse_filter_directive_line,
    resolve_dir_merge_path,
};

pub(crate) use executor::*;
pub use executor::{
    DestinationWriteGuard, PartialFileManager, PartialMode, SparseDetector, SparseReader,
    SparseRegion, SparseWriter, remove_existing_destination, remove_incomplete_destination,
};

pub(crate) use hard_links::HardLinkTracker;

pub(crate) use debug_flist::FlistTracer;

pub(crate) use metadata_sync::map_metadata_error;

#[cfg(all(unix, feature = "acl"))]
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

// ---------------------------------------------------------------------------
// Adaptive buffer sizing
// ---------------------------------------------------------------------------

/// Buffer size for files smaller than 64 KB (8 KB).
const ADAPTIVE_BUFFER_TINY: usize = 8 * 1024;
/// Buffer size for files in the 64 KB .. 1 MB range (32 KB).
const ADAPTIVE_BUFFER_SMALL: usize = 32 * 1024;
/// Buffer size for files in the 1 MB .. 64 MB range (128 KB).
const ADAPTIVE_BUFFER_MEDIUM: usize = 128 * 1024;
/// Buffer size for files 64 MB and larger (512 KB).
const ADAPTIVE_BUFFER_LARGE: usize = 512 * 1024;

/// File-size threshold below which [`ADAPTIVE_BUFFER_TINY`] is used (64 KB).
const ADAPTIVE_THRESHOLD_TINY: u64 = 64 * 1024;
/// File-size threshold below which [`ADAPTIVE_BUFFER_SMALL`] is used (1 MB).
const ADAPTIVE_THRESHOLD_SMALL: u64 = 1024 * 1024;
/// File-size threshold below which [`ADAPTIVE_BUFFER_MEDIUM`] is used (64 MB).
const ADAPTIVE_THRESHOLD_MEDIUM: u64 = 64 * 1024 * 1024;

/// Selects an I/O buffer size appropriate for the given file size.
///
/// The returned size balances memory consumption against throughput:
///
/// | File size          | Buffer size |
/// |--------------------|-------------|
/// | < 64 KB            | 8 KB        |
/// | 64 KB .. < 1 MB    | 32 KB       |
/// | 1 MB .. < 64 MB    | 128 KB      |
/// | >= 64 MB           | 512 KB      |
#[must_use]
pub(crate) const fn adaptive_buffer_size(file_size: u64) -> usize {
    if file_size < ADAPTIVE_THRESHOLD_TINY {
        ADAPTIVE_BUFFER_TINY
    } else if file_size < ADAPTIVE_THRESHOLD_SMALL {
        ADAPTIVE_BUFFER_SMALL
    } else if file_size < ADAPTIVE_THRESHOLD_MEDIUM {
        ADAPTIVE_BUFFER_MEDIUM
    } else {
        ADAPTIVE_BUFFER_LARGE
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
