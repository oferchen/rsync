//! Source operand execution entry points.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CopyOutcome, LocalCopyAction, LocalCopyArgumentError, LocalCopyError,
    LocalCopyExecution, LocalCopyMetadata, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord,
    LocalCopyRecordHandler, SourceSpec,
};
use crate::local_copy::{is_device, is_fifo};

use super::{
    copy_device, copy_directory_recursive, copy_fifo, copy_file, copy_symlink,
    follow_symlink_metadata, non_empty_path,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DestinationState {
    exists: bool,
    is_dir: bool,
    symlink_to_dir: bool,
}

/// Template-style helper to record how a skipped symlink was handled.
///
/// This centralizes the "skip symlink" behavior so the main copy loop
/// stays focused on control flow and delegates the reporting details.
fn record_skipped_symlink(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &fs::Metadata,
    record_path: Option<&Path>,
) {
    context.summary_mut().record_symlink_total();
    if let Some(relative_path) = record_path {
        match fs::read_link(source_path) {
            Ok(target) => {
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
                let record = LocalCopyRecord::new(
                    relative_path.to_path_buf(),
                    LocalCopyAction::SkippedNonRegular,
                    0,
                    None,
                    Duration::default(),
                    Some(metadata_snapshot),
                );
                context.record(record);
            }
            Err(_) => {
                context.record_skipped_non_regular(Some(relative_path));
            }
        }
    }
}

pub(crate) fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    handler: Option<&mut dyn LocalCopyRecordHandler>,
) -> Result<CopyOutcome, LocalCopyError> {
    let destination_root = plan.destination_spec().path().to_path_buf();
    let mut context = CopyContext::new(mode, options, handler, destination_root);
    let result = {
        let context = &mut context;
        (|| -> Result<(), LocalCopyError> {
            let multiple_sources = plan.sources().len() > 1;
            let destination_path = plan.destination_spec().path();
            let mut destination_state = query_destination_state(destination_path)?;
            if context.keep_dirlinks_enabled() && destination_state.symlink_to_dir {
                destination_state.is_dir = true;
            }

            if plan.destination_spec().force_directory() {
                ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                )?;
            }

            if multiple_sources {
                ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                )?;
            }

            let destination_behaves_like_directory =
                destination_state.is_dir || plan.destination_spec().force_directory();

            let relative_enabled = context.relative_paths_enabled();

            for source in plan.sources() {
                context.enforce_timeout()?;
                let source_path = source.path();
                let metadata_start = Instant::now();
                let relative_root = if relative_enabled {
                    source.relative_root()
                } else {
                    None
                };
                let relative_root = relative_root.filter(|path| !path.as_os_str().is_empty());
                let relative_parent = relative_root
                    .as_ref()
                    .and_then(|root| root.parent().map(|parent| parent.to_path_buf()))
                    .filter(|parent| !parent.as_os_str().is_empty());

                let metadata = match fs::symlink_metadata(source_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        context.record_file_list_generation(metadata_start.elapsed());
                        if context.delete_missing_args_enabled() {
                            delete_missing_source_entry(
                                context,
                                source,
                                destination_path,
                                destination_behaves_like_directory,
                                multiple_sources,
                                relative_root.as_deref(),
                            )?;
                            continue;
                        }

                        if context.ignore_missing_args_enabled() {
                            continue;
                        }

                        return Err(LocalCopyError::io(
                            "access source",
                            source_path.to_path_buf(),
                            error,
                        ));
                    }
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "access source",
                            source_path.to_path_buf(),
                            error,
                        ));
                    }
                };
                context.record_file_list_generation(metadata_start.elapsed());
                let record_relative = if metadata.file_type().is_dir() && source.copy_contents() {
                    None
                } else if let Some(root) = relative_root.as_ref() {
                    non_empty_path(root.as_path()).map(Path::to_path_buf)
                } else {
                    source_path
                        .file_name()
                        .map(|name| PathBuf::from(Path::new(name)))
                };
                context.record_file_list_entry(record_relative.as_deref());
                let file_type = metadata.file_type();
                let metadata_options = context.metadata_options();

                let root_device = if context.one_file_system_enabled() {
                    device_identifier(source_path, &metadata)
                } else {
                    None
                };

                let recursion_enabled = context.recursive_enabled();
                let dirs_enabled = context.dirs_enabled();

                let requires_directory_destination = relative_parent.is_some()
                    || (relative_root.is_some() && (source.copy_contents() || file_type.is_dir()));

                if requires_directory_destination && !destination_behaves_like_directory {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::DestinationMustBeDirectory,
                    ));
                }

                let destination_base = if let Some(parent) = &relative_parent {
                    destination_path.join(parent)
                } else {
                    destination_path.to_path_buf()
                };

                if file_type.is_dir() {
                    if source.copy_contents() {
                        if let Some(root) = relative_root.as_ref() {
                            if !context.allows(root.as_path(), true) {
                                continue;
                            }
                        }

                        if !recursion_enabled && !dirs_enabled {
                            let skip_relative = relative_root
                                .as_ref()
                                .and_then(|root| non_empty_path(root.as_path()));
                            context.summary_mut().record_directory_total();
                            context.record_skipped_directory(skip_relative);
                            continue;
                        }

                        let mut target_root = destination_path.to_path_buf();
                        if let Some(root) = &relative_root {
                            target_root = destination_path.join(root);
                        }

                        copy_directory_recursive(
                            context,
                            source_path,
                            &target_root,
                            &metadata,
                            relative_root
                                .as_ref()
                                .and_then(|root| non_empty_path(root.as_path())),
                            root_device,
                        )?;
                        continue;
                    }

                    let name = source_path.file_name().ok_or_else(|| {
                        LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::DirectoryNameUnavailable,
                        )
                    })?;
                    let relative = relative_root
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(Path::new(name)));
                    if !context.allows(&relative, true) {
                        continue;
                    }

                    if !recursion_enabled && !dirs_enabled {
                        context.summary_mut().record_directory_total();
                        let record_relative = non_empty_path(relative.as_path());
                        context.record_skipped_directory(record_relative);
                        continue;
                    }

                    let target = if destination_behaves_like_directory || multiple_sources {
                        destination_base.join(name)
                    } else {
                        destination_path.to_path_buf()
                    };

                    copy_directory_recursive(
                        context,
                        source_path,
                        &target,
                        &metadata,
                        non_empty_path(relative.as_path()),
                        root_device,
                    )?;
                } else {
                    let name = source_path.file_name().ok_or_else(|| {
                        LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::FileNameUnavailable,
                        )
                    })?;
                    let relative = relative_root
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(Path::new(name)));
                    let followed_metadata = if file_type.is_symlink()
                        && (context.copy_links_enabled() || context.copy_dirlinks_enabled())
                    {
                        match follow_symlink_metadata(source_path) {
                            Ok(target_metadata) => Some(target_metadata),
                            Err(error) => {
                                if context.copy_links_enabled() {
                                    return Err(error);
                                }
                                None
                            }
                        }
                    } else {
                        None
                    };

                    let (effective_metadata, effective_type) =
                        if let Some(ref target_metadata) = followed_metadata {
                            let ty = target_metadata.file_type();
                            if context.copy_links_enabled()
                                || (context.copy_dirlinks_enabled() && ty.is_dir())
                            {
                                (target_metadata, ty)
                            } else {
                                (&metadata, file_type)
                            }
                        } else {
                            (&metadata, file_type)
                        };

                    if !context.allows(&relative, effective_type.is_dir()) {
                        continue;
                    }

                    let prefer_root_destination = destination_behaves_like_directory
                        && context.force_replacements_enabled()
                        && !multiple_sources
                        && !plan.destination_spec().force_directory()
                        && relative_parent.is_none()
                        && !source.copy_contents();

                    let target = if destination_behaves_like_directory
                        && !(prefer_root_destination && !effective_type.is_dir())
                    {
                        destination_base.join(name)
                    } else {
                        destination_path.to_path_buf()
                    };

                    let record_path = non_empty_path(relative.as_path());
                    if effective_type.is_file() {
                        copy_file(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            record_path,
                        )?;
                    } else if effective_type.is_dir() {
                        copy_directory_recursive(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            non_empty_path(relative.as_path()),
                            root_device,
                        )?;
                    } else if file_type.is_symlink() {
                        if context.links_enabled() {
                            let target = if destination_behaves_like_directory
                                && !(prefer_root_destination)
                            {
                                destination_base.join(name)
                            } else {
                                destination_path.to_path_buf()
                            };

                            copy_symlink(
                                context,
                                source_path,
                                &target,
                                &metadata,
                                &metadata_options,
                                record_path,
                            )?;
                        } else {
                            record_skipped_symlink(context, source_path, &metadata, record_path);
                        }
                    } else if is_fifo(&effective_type) {
                        if !context.specials_enabled() {
                            context.record_skipped_non_regular(record_path);
                            continue;
                        }
                        let target =
                            if destination_behaves_like_directory && !prefer_root_destination {
                                destination_base.join(name)
                            } else {
                                destination_path.to_path_buf()
                            };

                        copy_fifo(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            &metadata_options,
                            record_path,
                        )?;
                    } else if is_device(&effective_type) {
                        if context.copy_devices_as_files_enabled() {
                            let target =
                                if destination_behaves_like_directory && !prefer_root_destination {
                                    destination_base.join(name)
                                } else {
                                    destination_path.to_path_buf()
                                };

                            copy_file(
                                context,
                                source_path,
                                &target,
                                effective_metadata,
                                record_path,
                            )?;
                        } else if !context.devices_enabled() {
                            context.record_skipped_non_regular(record_path);
                            continue;
                        } else {
                            let target =
                                if destination_behaves_like_directory && !prefer_root_destination {
                                    destination_base.join(name)
                                } else {
                                    destination_path.to_path_buf()
                                };

                            copy_device(
                                context,
                                source_path,
                                &target,
                                effective_metadata,
                                &metadata_options,
                                record_path,
                            )?;
                        }
                    } else {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::UnsupportedFileType,
                        ));
                    }
                }

                context.enforce_timeout()?;
            }

            context.flush_deferred_updates()?;
            context.flush_deferred_deletions()?;
            context.enforce_timeout()?;
            Ok(())
        })()
    };

    match result {
        Ok(()) => Ok(context.into_outcome()),
        Err(error) => {
            context.rollback_on_error(&error);
            Err(error)
        }
    }
}

fn delete_missing_source_entry(
    context: &mut CopyContext,
    source: &SourceSpec,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    relative_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    if source.copy_contents() {
        return Ok(());
    }

    let source_path = source.path();
    let relative = if let Some(root) = relative_root {
        root.to_path_buf()
    } else {
        let name = source_path.file_name().ok_or_else(|| {
            LocalCopyError::invalid_argument(LocalCopyArgumentError::FileNameUnavailable)
        })?;
        PathBuf::from(Path::new(name))
    };

    let target = if destination_behaves_like_directory || multiple_sources {
        destination_path.join(&relative)
    } else {
        destination_path.to_path_buf()
    };

    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect destination entry",
                target.clone(),
                error,
            ));
        }
    };

    let file_type = metadata.file_type();

    if !context.allows_deletion(relative.as_path(), file_type.is_dir()) {
        return Ok(());
    }

    if let Some(limit) = context.options().max_deletion_limit() {
        if context.summary().items_deleted() >= limit {
            return Err(LocalCopyError::delete_limit_exceeded(1));
        }
    }

    let record_path = non_empty_path(relative.as_path());

    if context.mode().is_dry_run() {
        context.summary_mut().record_deletion();
        if let Some(path) = record_path {
            context.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
        context.register_progress();
        return Ok(());
    }

    context.backup_existing_entry(&target, record_path, file_type)?;
    let removal = if file_type.is_dir() {
        fs::remove_dir_all(&target)
    } else {
        fs::remove_file(&target)
    };

    match removal {
        Ok(()) => {
            context.summary_mut().record_deletion();
            if let Some(path) = record_path {
                context.record(LocalCopyRecord::new(
                    path.to_path_buf(),
                    LocalCopyAction::EntryDeleted,
                    0,
                    None,
                    Duration::default(),
                    None,
                ));
            }
            context.register_progress();
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let action = if file_type.is_dir() {
                "remove destination directory"
            } else {
                "remove destination entry"
            };
            return Err(LocalCopyError::io(action, target, error));
        }
    }

    Ok(())
}

pub(crate) fn query_destination_state(path: &Path) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            let symlink_to_dir = if file_type.is_symlink() {
                follow_symlink_metadata(path)
                    .map(|target| target.file_type().is_dir())
                    .unwrap_or(false)
            } else {
                false
            };

            Ok(DestinationState {
                exists: true,
                is_dir: file_type.is_dir(),
                symlink_to_dir,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::default()),
        Err(error) => Err(LocalCopyError::io(
            "inspect destination",
            path.to_path_buf(),
            error,
        )),
    }
}

pub(crate) fn ensure_destination_directory(
    destination_path: &Path,
    state: &mut DestinationState,
    mode: LocalCopyExecution,
) -> Result<(), LocalCopyError> {
    if state.exists {
        if !state.is_dir {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::DestinationMustBeDirectory,
            ));
        }
        return Ok(());
    }

    if mode.is_dry_run() {
        state.exists = true;
        state.is_dir = true;
        return Ok(());
    }

    fs::create_dir_all(destination_path).map_err(|error| {
        LocalCopyError::io(
            "create destination directory",
            destination_path.to_path_buf(),
            error,
        )
    })?;
    state.exists = true;
    state.is_dir = true;
    Ok(())
}
