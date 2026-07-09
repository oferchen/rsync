//! Execution primitives for local copy operations.

mod cleanup;
mod directory;
mod file;
mod iconv;
mod reference;
mod sources;
mod special;
mod util;

pub(crate) use cleanup::{
    delete_extraneous_entries, record_directory_subtree, remove_source_entry_if_requested,
};
pub(crate) use directory::ChecksumCache;
pub(crate) use directory::{
    capture_batch_file_entry, copy_directory_recursive, copy_directory_walk_one_level, is_device,
    is_fifo,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use file::take_fsync_call_count;
pub(crate) use file::{
    CopyComparison, DEFAULT_XXH64_DEDUP_SIZE_LIMIT, SparseWriteState, copy_entry_to_backup,
    copy_file, should_skip_copy, system_time_within_window, write_sparse_chunk,
};
pub use file::{
    DestinationWriteGuard, PartialFileManager, PartialMode, SparseDetectStrategy, SparseDetector,
    SparseReader, SparseRegion, SparseWriteStats, SparseWriter, ZeroScanStrategy,
    compute_backup_path, remove_existing_destination, remove_incomplete_destination,
    trace_make_backup_copy, trace_make_backup_device, trace_make_backup_hlink,
    trace_make_backup_rename, trace_make_backup_symlink,
};
#[cfg(test)]
pub(crate) use file::{
    files_checksum_match, maybe_preallocate_destination, partial_destination_path,
    temporary_destination_path,
};
pub(crate) use iconv::{
    emit_cannot_convert_filename, name_is_convertible, transcode_filename_component,
};
pub(crate) use reference::{
    ReferenceDecision, ReferenceQuery, find_compare_dest_symlink, find_copy_dest_basis,
    find_copy_dest_symlink, find_reference_action,
};
pub(crate) use sources::copy_sources;
pub(crate) use special::{
    copy_device, copy_fifo, copy_symlink, create_symlink, symlink_target_is_safe,
};
pub(crate) use util::{follow_symlink_metadata, non_empty_path};
