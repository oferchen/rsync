//! Pre-copy skip checks for `--existing`, `--ignore-existing` and `--update`.
//!
//! Records the existence-gate skip decided by [`crate::existence_gate`] and
//! evaluates whether a newer destination suppresses the copy.
//!
//! upstream: generator.c:recv_generator() - existence and update checks

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::existence_gate::ExistenceSkip;
use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyError, LocalCopyMetadata, LocalCopyRecord,
};

/// Records a regular file skipped by the `--existing` / `--ignore-existing`
/// gate.
///
/// upstream: generator.c:1757-1806 recv_generator() - "not creating new file"
/// for an absent destination, "%s exists" for a present one.
pub(super) fn record_existence_skip(
    context: &mut CopyContext,
    skip: ExistenceSkip,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
    fake_super: bool,
) {
    let (action, metadata_snapshot) = match skip {
        ExistenceSkip::NotCreatingNew => {
            context.summary_mut().record_regular_file_skipped_missing();
            let snapshot = LocalCopyMetadata::from_metadata(metadata, None)
                .virtualize_fake_super(source, fake_super);
            (LocalCopyAction::SkippedMissingDestination, snapshot)
        }
        ExistenceSkip::Exists => {
            context.summary_mut().record_regular_file_ignored_existing();
            context.record_hard_link(metadata, destination);
            let snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            (LocalCopyAction::SkippedExisting, snapshot)
        }
    };
    let total_bytes = Some(metadata_snapshot.len());
    context.record(LocalCopyRecord::new(
        record_path.to_path_buf(),
        action,
        0,
        total_bytes,
        Duration::default(),
        Some(metadata_snapshot),
    ));
}

/// Checks the `--update` condition and returns `true` if the file should be
/// skipped because the destination is newer.
pub(super) fn handle_update_skip(
    context: &mut CopyContext,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
    existing_metadata: Option<&fs::Metadata>,
) -> Result<bool, LocalCopyError> {
    if context.update_enabled()
        && let Some(existing) = existing_metadata
        && super::super::comparison::destination_is_newer(
            metadata,
            existing,
            context.options().modify_window(),
        )
    {
        context.summary_mut().record_regular_file_skipped_newer();
        context.record_hard_link(metadata, destination);
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::SkippedNewerDestination,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        return Ok(true);
    }

    Ok(false)
}
