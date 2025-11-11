use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::local_copy::remove_existing_destination;
#[cfg(all(unix, feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{
    map_metadata_error, overrides::create_hard_link, remove_source_entry_if_requested, CopyContext,
    CreatedEntryKind, LocalCopyAction, LocalCopyArgumentError, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord,
};
use ::metadata::{apply_file_metadata_with_options, MetadataOptions};
#[cfg(unix)]
use ::metadata::create_fifo;

pub(crate) fn copy_fifo(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let mode = context.mode();
    let file_type = metadata.file_type();

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(unix, feature = "acl"))]
    let preserve_acls = context.acls_enabled();

    #[cfg(not(all(unix, feature = "xattr")))]
    let _ = context;
    #[cfg(not(all(unix, feature = "acl")))]
    let _ = mode;

    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));

    context.summary_mut().record_fifo_total();

    // --existing / --ignore-non-existing
    if context.existing_only_enabled() {
        match fs::symlink_metadata(destination) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(path) = &record_path {
                    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                    context.record(LocalCopyRecord::new(
                        path.clone(),
                        LocalCopyAction::SkippedMissingDestination,
                        0,
                        Some(metadata_snapshot.len()),
                        Duration::default(),
                        Some(metadata_snapshot),
                    ));
                }
                return Ok(());
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }
    }

    // could be deduped to an earlier FIFO we created
    let mut existing_hard_link_target = context.existing_hard_link_target(metadata);

    // ensure parent exists
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
                context.register_progress();
            }
        }
    }

    // dry-run path: just validate and record
    if mode.is_dry_run() {
        match fs::symlink_metadata(destination) {
            Ok(existing) => {
                if existing.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if existing_hard_link_target.is_some() {
            context.summary_mut().record_hard_link();
        } else {
            context.summary_mut().record_fifo();
        }

        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            let action = if existing_hard_link_target.is_some() {
                LocalCopyAction::HardLink
            } else {
                LocalCopyAction::FifoCopied
            };
            context.record(LocalCopyRecord::new(
                path.clone(),
                action,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }

        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    // real copy
    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
            if existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                ));
            }

            context.backup_existing_entry(destination, relative, existing.file_type())?;
            remove_existing_destination(destination)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    // try to hard-link to an earlier FIFO we made
    if let Some(link_source) = existing_hard_link_target.take() {
        match create_hard_link(&link_source, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_existing_destination(destination)?;
                create_hard_link(&link_source, destination).map_err(|link_error| {
                    LocalCopyError::io("create hard link", destination.to_path_buf(), link_error)
                })?;
            }
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code) if code == crate::local_copy::CROSS_DEVICE_ERROR_CODE
                ) =>
            {
                // degrade to "recreate fifo" below
                existing_hard_link_target = Some(link_source);
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create hard link",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if existing_hard_link_target.is_none() {
            apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
                .map_err(map_metadata_error)?;
            #[cfg(all(unix, feature = "xattr"))]
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                context.filter_program(),
            )?;
            #[cfg(all(unix, feature = "acl"))]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

            context.record_hard_link(metadata, destination);
            context.summary_mut().record_hard_link();

            if let Some(path) = &record_path {
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                let total_bytes = Some(metadata_snapshot.len());
                context.record(LocalCopyRecord::new(
                    path.clone(),
                    LocalCopyAction::HardLink,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                ));
            }

            context.register_created_path(
                destination,
                CreatedEntryKind::HardLink,
                destination_previously_existed,
            );
            context.register_progress();
            remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
            return Ok(());
        }
    }

    // actually create a FIFO
    #[cfg(unix)]
    {
        create_fifo(destination, metadata).map_err(map_metadata_error)?;
    }
    #[cfg(not(unix))]
    {
        // Windows / non-Unix: no FIFO support in this crate path.
        // Create an empty file so the caller sees a tangible result and the rest of the
        // metadata application doesn’t fail.
        fs::File::create(destination).map_err(|error| {
            LocalCopyError::io("create fifo placeholder", destination.to_path_buf(), error)
        })?;
    }

    context.register_created_path(
        destination,
        CreatedEntryKind::Fifo,
        destination_previously_existed,
    );

    apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;
    #[cfg(all(unix, feature = "acl"))]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_fifo();

    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::FifoCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }

    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
    Ok(())
}
