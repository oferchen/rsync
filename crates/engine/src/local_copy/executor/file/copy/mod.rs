mod dry_run;
mod existing;
mod links;
mod transfer;

use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::{CopyContext, LocalCopyArgumentError, LocalCopyError};

use transfer::execute_transfer;

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
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
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
    if let Some(parent) = destination.parent() {
        context.prepare_parent_directory(parent)?;
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

    if mode.is_dry_run() {
        dry_run::handle_dry_run(
            context,
            source,
            destination,
            metadata,
            record_path.as_path(),
            existing_metadata.as_ref(),
            destination_previously_existed,
            file_size,
            file_type,
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

    let use_sparse_writes = context.sparse_enabled();
    let partial_enabled = context.partial_enabled();
    let inplace_enabled = context.inplace_enabled();
    let checksum_enabled = context.checksum_enabled();
    let size_only_enabled = context.size_only_enabled();
    let append_allowed = context.append_enabled();
    let append_verify = context.append_verify_enabled();
    let whole_file_enabled = context.whole_file_enabled();
    let compress_enabled = context.should_compress(record_path.as_path());
    let relative_for_link = relative.unwrap_or(record_path.as_path());

    if let Some(existing) = existing_metadata.as_ref() {
        context.backup_existing_entry(destination, relative, existing.file_type())?;
    }

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
        checksum_enabled,
        mode,
        #[cfg(feature = "xattr")]
        preserve_xattrs,
        #[cfg(feature = "acl")]
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
        checksum_enabled,
        mode,
        #[cfg(feature = "xattr")]
        preserve_xattrs,
        #[cfg(feature = "acl")]
        preserve_acls,
        link_outcome.copy_source_override,
    )
}
