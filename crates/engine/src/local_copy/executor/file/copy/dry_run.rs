use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyError, LocalCopyMetadata, LocalCopyRecord,
    remove_source_entry_if_requested,
};

use super::super::append::{AppendMode, determine_append_mode};

pub(super) fn handle_dry_run(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
    existing_metadata: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    file_size: u64,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    if context.update_enabled() {
        if let Some(existing) = existing_metadata {
            if super::super::comparison::destination_is_newer(metadata, existing) {
                context.summary_mut().record_regular_file_skipped_newer();
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
                return Ok(());
            }
        }
    }

    if context.ignore_existing_enabled() && existing_metadata.is_some() {
        context.summary_mut().record_regular_file_ignored_existing();
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
        return Ok(());
    }

    let mut reader = fs::File::open(source)
        .map_err(|error| LocalCopyError::io("open source file", source.to_path_buf(), error))?;

    let append_mode = determine_append_mode(
        context.append_enabled(),
        context.append_verify_enabled(),
        &mut reader,
        source,
        destination,
        existing_metadata,
        file_size,
    )?;
    let append_offset = match append_mode {
        AppendMode::Append(offset) => offset,
        AppendMode::Disabled => 0,
    };
    let bytes_transferred = file_size.saturating_sub(append_offset);

    context
        .summary_mut()
        .record_file(file_size, bytes_transferred, None);
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    context.record(
        LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::DataCopied,
            bytes_transferred,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        )
        .with_creation(!destination_previously_existed),
    );
    remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
    Ok(())
}
