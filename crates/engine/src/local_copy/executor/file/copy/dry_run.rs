//! Dry-run file copy simulation.
//!
//! Records what would happen during a real transfer without performing any
//! I/O. Produces the same [`LocalCopyRecord`] events so that itemized
//! output (`-i`) and statistics match a real run.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyChangeSet, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord, ReferenceDecision, ReferenceQuery, find_reference_action,
    remove_source_entry_if_requested,
};

use super::super::append::{AppendMode, determine_append_mode};

/// Aggregated parameters for simulating a file copy in dry-run mode.
pub(super) struct DryRunRequest<'a> {
    pub source: &'a Path,
    pub destination: &'a Path,
    pub metadata: &'a fs::Metadata,
    pub record_path: &'a Path,
    pub existing_metadata: Option<&'a fs::Metadata>,
}

/// Processes a file copy in dry-run mode without writing any data.
///
/// Records the transfer in the summary and event log, respecting
/// `--update`, `--ignore-existing`, and `--append` semantics.
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
        && super::super::comparison::destination_is_newer(
            metadata,
            existing,
            context.options().modify_window(),
        )
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

    // upstream: generator.c:1469-1483 - in dry-run mode the generator still
    // evaluates `--link-dest` / `--copy-dest` / `--compare-dest` and itemizes
    // the file against the matched basis (`hf`/`cf`/`.f` + blank) rather than
    // reporting a full transfer (`>f+++++++++`). Mirror that here so a dry-run
    // produces the same itemize stream as the real run.
    if existing_metadata.is_none()
        && simulate_reference_match(context, source, destination, metadata, record_path)?
    {
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

    // upstream: main.c:1839-1840 - `--only-write-batch` forces dry_run=1 but the
    // batch_fd capture path still runs so the recorded stream contains the
    // file's token data. Mirror that by capturing the whole file into the per-
    // file batch delta buffer and finalising it (token end + xfer checksum)
    // whenever a batch writer is active. Without this, `--only-write-batch`
    // emits a batch file with flist entries but no delta payload, and the
    // matching `--read-batch` reconstructs zero file content.
    context.capture_batch_whole_file(source, file_size)?;
    context.finalize_batch_file_delta(source)?;

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

/// Records the dry-run itemize for a regular file that would be satisfied by a
/// `--link-dest` / `--copy-dest` / `--compare-dest` basis or an in-transfer
/// hard-link leader, without performing any I/O.
///
/// Returns `true` when a match was recorded, mirroring the real-run itemize:
/// `hf` for a link-dest hardlink or an intra-transfer alias, `cf` for a
/// copy-dest reconstruction, and `.f` for a compare-dest match. The change set
/// is computed against the basis so identical files leave the columns blank.
fn simulate_reference_match(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
) -> Result<bool, LocalCopyError> {
    let size_only = context.size_only_enabled();
    let ignore_times = context.ignore_times_enabled();
    let checksum = context.checksum_enabled();
    let metadata_options = context.metadata_options();

    // An intra-transfer hard-link leader placed earlier this run takes
    // precedence: the alias itemizes as `hf <path> => <leader>`.
    if let Some(leader) = context.existing_hard_link_target(metadata) {
        let leader_display = leader
            .strip_prefix(context.destination_root())
            .map(std::path::Path::to_path_buf)
            .ok()
            .filter(|relative| relative != record_path);
        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, leader_display);
        record_match(
            context,
            record_path,
            LocalCopyAction::HardLink,
            &metadata_snapshot,
        );
        return Ok(true);
    }

    // --link-dest hardlink of a matching basis file.
    if context
        .link_dest_target(
            record_path,
            source,
            metadata,
            size_only,
            ignore_times,
            checksum,
        )?
        .is_some()
    {
        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        record_match(
            context,
            record_path,
            LocalCopyAction::HardLink,
            &metadata_snapshot,
        );
        return Ok(true);
    }

    // --copy-dest / --compare-dest matches.
    let query = ReferenceQuery {
        destination,
        relative: record_path,
        source,
        metadata,
        size_only,
        ignore_times,
        checksum,
    };
    let Some(decision) = find_reference_action(context, query)? else {
        return Ok(false);
    };

    let (action, basis) = match decision {
        ReferenceDecision::Skip => (LocalCopyAction::MetadataReused, None),
        ReferenceDecision::Copy(path) => (LocalCopyAction::ReferenceCopied, Some(path)),
        // A degraded link-dest copy still itemizes as a local change.
        ReferenceDecision::Link(path) => (LocalCopyAction::ReferenceCopied, Some(path)),
    };
    context.summary_mut().record_regular_file_matched();
    let change_set = match basis.as_deref() {
        Some(path) => {
            let basis_metadata = fs::symlink_metadata(path)
                .map_err(|error| LocalCopyError::io("inspect reference basis", path, error))?;
            LocalCopyChangeSet::for_file(
                metadata,
                Some(&basis_metadata),
                &metadata_options,
                true,
                false,
                false,
                false,
            )
        }
        None => {
            // compare-dest: itemize against the basis the same way the real run
            // does (no transfer, blank columns when identical).
            LocalCopyChangeSet::new()
        }
    };
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    context.record(
        LocalCopyRecord::new(
            record_path.to_path_buf(),
            action,
            0,
            Some(metadata_snapshot.len()),
            Duration::default(),
            Some(metadata_snapshot),
        )
        .with_change_set(change_set),
    );
    Ok(true)
}

/// Records a hard-link dry-run match with no attribute drift.
fn record_match(
    context: &mut CopyContext,
    record_path: &Path,
    action: LocalCopyAction,
    metadata_snapshot: &LocalCopyMetadata,
) {
    context.record(LocalCopyRecord::new(
        record_path.to_path_buf(),
        action,
        0,
        Some(metadata_snapshot.len()),
        Duration::default(),
        Some(metadata_snapshot.clone()),
    ));
}
