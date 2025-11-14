mod dry_run;
mod existing;
mod links;
mod transfer;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyArgumentError, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord,
};

use transfer::execute_transfer;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use transfer::take_fsync_call_count;

pub(crate) fn copy_file(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let metadata_options = context.metadata_options();
    let mode = context.mode();
    let file_type = metadata.file_type();

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(unix, feature = "acl"))]
    let preserve_acls = context.acls_enabled();

    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| source.file_name().map(PathBuf::from))
        .unwrap_or_else(|| {
            destination
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default()
        });
    let file_size = metadata.len();
    context.summary_mut().record_regular_file_total();
    context.summary_mut().record_total_bytes(file_size);

    if let Some(min_limit) = context.min_file_size_limit() {
        if file_size < min_limit {
            return Ok(());
        }
    }

    if let Some(max_limit) = context.max_file_size_limit() {
        if file_size > max_limit {
            return Ok(());
        }
    }

    let existing_metadata = match fs::symlink_metadata(destination) {
        Ok(existing) => Some(existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    };

    if let Some(existing) = &existing_metadata {
        if existing.file_type().is_dir() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceDirectoryWithFile,
            ));
        }
    }

    let destination_previously_existed = existing_metadata.is_some();

    if context.existing_only_enabled() && existing_metadata.is_none() {
        context.summary_mut().record_regular_file_skipped_missing();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::SkippedMissingDestination,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        context.prepare_parent_directory(parent)?;
    }

    if mode.is_dry_run() {
        dry_run::handle_dry_run(
            context,
            dry_run::DryRunRequest {
                source,
                destination,
                metadata,
                record_path: record_path.as_path(),
                existing_metadata: existing_metadata.as_ref(),
            },
        )?;
        return Ok(());
    }

    if existing::handle_existing_skips(
        context,
        destination,
        metadata,
        record_path.as_path(),
        existing_metadata.as_ref(),
    )? {
        return Ok(());
    }

    // Upstream rsync disables sparse writes whenever `--preallocate` is active
    // because the preallocation request must materialise every range in the
    // destination file.  Keep the same behaviour by refusing to punch holes
    // when preallocation is enabled.
    let use_sparse_writes = context.sparse_enabled() && !context.preallocate_enabled();
    let partial_enabled = context.partial_enabled();
    let inplace_enabled = context.inplace_enabled();
    let checksum_enabled = context.checksum_enabled();
    let size_only_enabled = context.size_only_enabled();
    let ignore_times_enabled = context.ignore_times_enabled();
    let append_allowed = context.append_enabled();
    let append_verify = context.append_verify_enabled();
    let whole_file_enabled = context.whole_file_enabled();
    let compress_enabled = context.should_compress(record_path.as_path());
    let relative_for_link = relative.unwrap_or(record_path.as_path());

    let link_outcome = links::process_links(
        context,
        source,
        destination,
        metadata,
        record_path.as_path(),
        relative_for_link,
        metadata_options.clone(),
        existing_metadata.as_ref(),
        destination_previously_existed,
        file_type,
        size_only_enabled,
        ignore_times_enabled,
        checksum_enabled,
        mode,
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(unix, feature = "acl"))]
        preserve_acls,
    )?;

    if link_outcome.completed {
        return Ok(());
    }

    execute_transfer(
        context,
        source,
        destination,
        metadata,
        metadata_options,
        record_path.as_path(),
        existing_metadata.as_ref(),
        destination_previously_existed,
        file_type,
        relative,
        append_allowed,
        append_verify,
        whole_file_enabled,
        inplace_enabled,
        partial_enabled,
        use_sparse_writes,
        compress_enabled,
        size_only_enabled,
        ignore_times_enabled,
        checksum_enabled,
        mode,
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(unix, feature = "acl"))]
        preserve_acls,
        link_outcome.copy_source_override,
    )
}
