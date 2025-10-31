//! Source operand execution entry points.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CopyOutcome, LocalCopyArgumentError, LocalCopyError, LocalCopyExecution,
    LocalCopyOptions, LocalCopyPlan, LocalCopyRecordHandler,
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
                let metadata = match fs::symlink_metadata(source_path) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            && context.ignore_missing_args_enabled() =>
                    {
                        context.record_file_list_generation(metadata_start.elapsed());
                        continue;
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
                let file_type = metadata.file_type();
                let metadata_options = context.metadata_options();

                let root_device = if context.one_file_system_enabled() {
                    device_identifier(source_path, &metadata)
                } else {
                    None
                };

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

                    let target = if destination_behaves_like_directory {
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
                    } else if file_type.is_symlink() && !context.copy_links_enabled() {
                        copy_symlink(
                            context,
                            source_path,
                            &target,
                            &metadata,
                            &metadata_options,
                            record_path,
                        )?;
                    } else if is_fifo(&effective_type) {
                        if !context.specials_enabled() {
                            context.record_skipped_non_regular(record_path);
                            continue;
                        }
                        copy_fifo(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            &metadata_options,
                            record_path,
                        )?;
                    } else if is_device(&effective_type) {
                        if !context.devices_enabled() {
                            context.record_skipped_non_regular(record_path);
                            continue;
                        }
                        copy_device(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            &metadata_options,
                            record_path,
                        )?;
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
