//! Execution primitives for local copy operations.

mod cleanup;
mod directory;
mod file;
mod reference;
mod sources;
mod special;
mod util;

pub(crate) use cleanup::{delete_extraneous_entries, remove_source_entry_if_requested};
#[cfg(feature = "parallel")]
pub(crate) use directory::ChecksumCache;
pub(crate) use directory::{copy_directory_recursive, is_device, is_fifo};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use file::take_fsync_call_count;
pub(crate) use file::{
    CopyComparison, SparseWriteState, compute_backup_path, copy_entry_to_backup, copy_file,
    should_skip_copy, write_sparse_chunk,
};
pub use file::{
    DestinationWriteGuard, PartialFileManager, PartialMode, SparseDetector, SparseReader,
    SparseRegion, SparseWriter, remove_existing_destination, remove_incomplete_destination,
};
#[cfg(test)]
pub(crate) use file::{
    files_checksum_match, maybe_preallocate_destination, partial_destination_path,
    temporary_destination_path,
};
pub(crate) use reference::{ReferenceDecision, ReferenceQuery, find_reference_action};
pub(crate) use sources::copy_sources;
pub(crate) use special::{
    copy_device, copy_fifo, copy_symlink, create_symlink, symlink_target_is_safe,
};
pub(crate) use util::{follow_symlink_metadata, non_empty_path};
