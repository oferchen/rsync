//! Regular file copy routines and helpers.

mod append;
mod backup;
mod comparison;
mod copy;
mod guard;
pub mod partial;
mod paths;
mod preallocate;
mod sparse;

pub(crate) use backup::{compute_backup_path, copy_entry_to_backup};
#[cfg(test)]
pub(crate) use comparison::files_checksum_match;
pub(crate) use comparison::{CopyComparison, should_skip_copy};
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
pub(crate) use preallocate::maybe_preallocate_destination;
pub use sparse::{SparseDetector, SparseReader, SparseRegion, SparseWriter};
pub(crate) use sparse::{SparseWriteState, write_sparse_chunk};
