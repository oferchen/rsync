use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::local_copy::overrides::create_hard_link;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet, LocalCopyError,
    LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord, ReferenceDecision, ReferenceQuery,
    find_reference_action, map_metadata_error, remove_source_entry_if_requested,
};

#[cfg(feature = "acl")]
use crate::local_copy::sync_acls_if_requested;
#[cfg(feature = "xattr")]
use crate::local_copy::sync_xattrs_if_requested;

use rsync_meta::MetadataOptions;
use rsync_meta::apply_file_metadata_with_options;

use super::super::super::super::CROSS_DEVICE_ERROR_CODE;
use super::super::guard::remove_existing_destination;

pub(super) struct LinkOutcome {
    pub(super) copy_source_override: Option<PathBuf>,
    pub(super) completed: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn process_links(
    context: &mut CopyContext<'_>,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    record_path: &Path,
    relative_for_link: &Path,
    metadata_options: MetadataOptions,
    existing_metadata: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    file_type: fs::FileType,
    size_only_enabled: bool,
    ignore_times_enabled: bool,
    checksum_enabled: bool,
    mode: LocalCopyExecution,
    #[cfg(feature = "xattr")] preserve_xattrs: bool,
    #[cfg(feature = "acl")] preserve_acls: bool,
) -> Result<LinkOutcome, LocalCopyError> {
    #[cfg(not(any(feature = "xattr", feature = "acl")))]
    let _ = mode;
    let mut copy_source_override: Option<PathBuf> = None;

    if let Some(link_target) = context.link_dest_target(
        relative_for_link,
        source,
        metadata,
        size_only_enabled,
        ignore_times_enabled,
        checksum_enabled,
    )? {
        let mut attempted_commit = false;
        loop {
            match fs::hard_link(&link_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(destination)?;
                    fs::hard_link(&link_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&link_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::HardLink,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
        return Ok(LinkOutcome {
            copy_source_override: None,
            completed: true,
        });
    }

    if let Some(existing_target) = context.existing_hard_link_target(metadata) {
        let mut attempted_commit = false;
        loop {
            match create_hard_link(&existing_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(destination)?;
                    create_hard_link(&existing_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&existing_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::HardLink,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        remove_source_entry_if_requested(context, source, Some(record_path), file_type)?;
        return Ok(LinkOutcome {
            copy_source_override: None,
            completed: true,
        });
    }

    if !context.reference_directories().is_empty() && !record_path.as_os_str().is_empty() {
        if let Some(decision) = find_reference_action(
            context,
            ReferenceQuery {
                destination,
                relative: record_path,
                source,
                metadata,
                size_only: size_only_enabled,
                ignore_times: ignore_times_enabled,
                checksum: checksum_enabled,
            },
        )? {
            match decision {
                ReferenceDecision::Skip => {
                    context.summary_mut().record_regular_file_matched();
                    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                    let total_bytes = Some(metadata_snapshot.len());
                    let xattrs_enabled = {
                        #[cfg(feature = "xattr")]
                        {
                            preserve_xattrs
                        }
                        #[cfg(not(feature = "xattr"))]
                        {
                            false
                        }
                    };
                    let acls_enabled = {
                        #[cfg(feature = "acl")]
                        {
                            preserve_acls
                        }
                        #[cfg(not(feature = "acl"))]
                        {
                            false
                        }
                    };
                    let change_set = LocalCopyChangeSet::for_file(
                        metadata,
                        existing_metadata,
                        &metadata_options,
                        true,
                        false,
                        xattrs_enabled,
                        acls_enabled,
                    );
                    context.record(
                        LocalCopyRecord::new(
                            record_path.to_path_buf(),
                            LocalCopyAction::MetadataReused,
                            0,
                            total_bytes,
                            Duration::default(),
                            Some(metadata_snapshot),
                        )
                        .with_change_set(change_set),
                    );
                    context.register_progress();
                    remove_source_entry_if_requested(
                        context,
                        source,
                        Some(record_path),
                        file_type,
                    )?;
                    return Ok(LinkOutcome {
                        copy_source_override: None,
                        completed: true,
                    });
                }
                ReferenceDecision::Copy(path) => {
                    copy_source_override = Some(path);
                }
                ReferenceDecision::Link(path) => {
                    if existing_metadata.is_some() {
                        remove_existing_destination(destination)?;
                    }

                    let link_result = create_hard_link(&path, destination);
                    let mut degrade_to_copy = false;
                    match link_result {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            remove_existing_destination(destination)?;
                            create_hard_link(&path, destination).map_err(|link_error| {
                                LocalCopyError::io(
                                    "create hard link",
                                    destination.to_path_buf(),
                                    link_error,
                                )
                            })?;
                        }
                        Err(error)
                            if matches!(
                                error.raw_os_error(),
                                Some(code) if code == CROSS_DEVICE_ERROR_CODE
                            ) =>
                        {
                            degrade_to_copy = true;
                        }
                        Err(error) => {
                            return Err(LocalCopyError::io(
                                "create hard link",
                                destination.to_path_buf(),
                                error,
                            ));
                        }
                    }

                    if degrade_to_copy {
                        copy_source_override = Some(path);
                    } else if copy_source_override.is_none() {
                        apply_file_metadata_with_options(
                            destination,
                            metadata,
                            metadata_options.clone(),
                        )
                        .map_err(map_metadata_error)?;
                        #[cfg(feature = "xattr")]
                        sync_xattrs_if_requested(
                            preserve_xattrs,
                            mode,
                            source,
                            destination,
                            true,
                            context.filter_program(),
                        )?;
                        #[cfg(feature = "acl")]
                        sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
                        context.record_hard_link(metadata, destination);
                        context.summary_mut().record_hard_link();
                        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                        let total_bytes = Some(metadata_snapshot.len());
                        context.record(LocalCopyRecord::new(
                            record_path.to_path_buf(),
                            LocalCopyAction::HardLink,
                            0,
                            total_bytes,
                            Duration::default(),
                            Some(metadata_snapshot),
                        ));
                        context.register_created_path(
                            destination,
                            CreatedEntryKind::HardLink,
                            destination_previously_existed,
                        );
                        context.register_progress();
                        remove_source_entry_if_requested(
                            context,
                            source,
                            Some(record_path),
                            file_type,
                        )?;
                        return Ok(LinkOutcome {
                            copy_source_override: None,
                            completed: true,
                        });
                    }
                }
            }
        }
    }

    Ok(LinkOutcome {
        copy_source_override,
        completed: false,
    })
}
