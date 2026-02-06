use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyError, LocalCopyMetadata, LocalCopyRecord,
    remove_source_entry_if_requested,
};

use super::super::append::{AppendMode, determine_append_mode};

pub(super) struct DryRunRequest<'a> {
    pub source: &'a Path,
    pub destination: &'a Path,
    pub metadata: &'a fs::Metadata,
    pub record_path: &'a Path,
    pub existing_metadata: Option<&'a fs::Metadata>,
}

pub(super) fn handle_dry_run(
    context: &mut CopyContext,
    request: DryRunRequest<'_>,
) -> Result<(), LocalCopyError> {
    let DryRunRequest {
        source,
        destination,
        metadata,
        record_path,
        existing_metadata,
    } = request;
    let destination_previously_existed = existing_metadata.is_some();
    let file_size = metadata.len();
    let file_type = metadata.file_type();
    if context.update_enabled()
        && let Some(existing) = existing_metadata
        && super::super::comparison::destination_is_newer(metadata, existing, context.options().modify_window())
    {
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
        .map_err(|error| LocalCopyError::io("open source file", source, error))?;

    let append_mode = determine_append_mode(
        context.append_enabled(),
        context.append_verify_enabled(),
        &mut reader,
        source,
        destination,
        existing_metadata,
        file_size,
    )?;
    if matches!(append_mode, AppendMode::Skip) {
        // Upstream rsync skips the file when dest >= source size in append mode.
        context.summary_mut().record_regular_file_matched();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::MetadataReused,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        return Ok(());
    }
    let append_offset = match append_mode {
        AppendMode::Append(offset) => offset,
        AppendMode::Disabled | AppendMode::Skip => 0,
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
