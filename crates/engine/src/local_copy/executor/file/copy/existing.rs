use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyError, LocalCopyMetadata, LocalCopyRecord,
};

pub(super) fn handle_existing_skips(
    context: &mut CopyContext,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
    existing_metadata: Option<&fs::Metadata>,
) -> Result<bool, LocalCopyError> {
    if context.update_enabled() {
        if let Some(existing) = existing_metadata {
            if super::super::comparison::destination_is_newer(metadata, existing) {
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
        }
    }

    if context.ignore_existing_enabled() && existing_metadata.is_some() {
        context.summary_mut().record_regular_file_ignored_existing();
        context.record_hard_link(metadata, destination);
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::SkippedExisting,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        return Ok(true);
    }

    Ok(false)
}
