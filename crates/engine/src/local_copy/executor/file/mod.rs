//! Regular file copy routines and helpers.

mod append;
mod backup;
mod backup_trace;
mod comparison;
mod copy;
mod guard;
/// Partial file management for interrupted transfers.
pub mod partial;
mod paths;
mod preallocate;
mod sparse;

pub use backup::compute_backup_path;
pub(crate) use backup::{copy_entry_to_backup, create_backup_parents};
pub use backup_trace::{
    trace_make_backup_copy, trace_make_backup_device, trace_make_backup_hlink,
    trace_make_backup_rename, trace_make_backup_symlink,
};
#[cfg(test)]
pub(crate) use comparison::files_checksum_match;
pub(crate) use comparison::{
    CopyComparison, DEFAULT_XXH64_DEDUP_SIZE_LIMIT, should_skip_copy, system_time_within_window,
};
pub(crate) use copy::copy_file;
#[cfg(test)]
pub(crate) use copy::take_fsync_call_count;
pub use guard::{
    DestinationWriteGuard, remove_existing_destination, remove_incomplete_destination,
};
pub use partial::{PartialFileManager, PartialMode};
#[cfg(test)]
pub(crate) use paths::partial_destination_path;
#[cfg(test)]
pub(crate) use paths::temp_name_with_suffix;
#[cfg(test)]
pub(crate) use preallocate::maybe_preallocate_destination;
pub use sparse::{
    SparseDetectStrategy, SparseDetector, SparseReader, SparseRegion, SparseWriteStats,
    SparseWriter, ZeroScanStrategy,
};
pub(crate) use sparse::{SparseWriteState, write_sparse_chunk};
