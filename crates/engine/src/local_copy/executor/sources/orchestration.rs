//! Top-level source processing orchestration and deferred operation flushing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CopyOutcome, LocalCopyAction, LocalCopyArgumentError, LocalCopyError,
    LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord, LocalCopyRecordHandler,
    SourceSpec,
};

use super::super::non_empty_path;
use super::destination::{ensure_destination_directory, query_destination_state};
use super::handlers::{
    handle_directory_contents_copy, handle_directory_copy, handle_non_directory_source,
};
use super::metadata::{compute_relative_paths, fetch_source_metadata};
use super::types::{SourceMetadataResult, SourceProcessingContext};

/// Entry point for copying all sources to the destination.
///
/// Sets up the copy context, iterates over sources, and handles deferred
/// operations and error rollback.
pub(crate) fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    handler: Option<&mut dyn LocalCopyRecordHandler>,
) -> Result<CopyOutcome, LocalCopyError> {
    // upstream: rsync.c:do_as_root() - switch effective UID/GID before receiver
    // file operations. The RAII guard restores the original identity on drop.
    let _copy_as_guard = options
        .copy_as_ids()
        .map(::metadata::switch_effective_ids)
        .transpose()
        .map_err(|err| {
            LocalCopyError::io(
                "switch effective identity for --copy-as",
                plan.destination_spec().path(),
                err,
            )
        })?;

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

            let mut first_io_error: Option<LocalCopyError> = None;
            for source in plan.sources() {
                let result = process_single_source(
                    context,
                    plan,
                    source,
                    destination_path,
                    destination_behaves_like_directory,
                    multiple_sources,
                );
                if let Err(error) = result {
                    if error.is_vanished_error() {
                        // upstream: flist.c:1289 - vanished files produce a warning
                        // and set IOERR_VANISHED, but transfer continues.
                        eprintln!("file has vanished: {}", source.path().display());
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else if error.is_io_error() {
                        // upstream: rsync continues transferring remaining sources
                        // when individual entries fail with I/O errors, regardless
                        // of whether --delete is active.
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else {
                        return Err(error);
                    }
                }
            }

            // Write the flist end-of-list marker, ID lists, then delta data.
            // upstream: flist.c:2513-2514 - without INC_RECURSE, send_id_lists()
            // writes uid/gid name mappings after the flist end marker.
            // Since names are already embedded inline via XMIT_USER_NAME_FOLLOWS,
            // the ID lists are empty (just varint30(0) terminators), but they
            // must be present for upstream's recv_id_list() to consume.
            context.finalize_batch_flist()?;
            context.write_batch_id_lists()?;
            context.flush_batch_delta_to_batch()?;
            // Stats are written by core::client::run::batch::finalize_batch()
            // after the engine returns, using actual transfer byte counts.

            flush_deferred_operations(context)?;

            if let Some(error) = first_io_error {
                return Err(error);
            }
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

/// Processes a single source entry in the copy operation.
fn process_single_source(
    context: &mut CopyContext,
    plan: &LocalCopyPlan,
    source: &SourceSpec,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
) -> Result<(), LocalCopyError> {
    // Directory copy handlers set the correct offset before recursing.
    context.set_safety_depth_offset(0);
    context.enforce_timeout()?;

    let source_path = source.path();
    let metadata_start = Instant::now();

    let (relative_root, relative_parent) = compute_relative_paths(context, source);

    let metadata_result = fetch_source_metadata(
        context,
        source,
        source_path,
        destination_path,
        destination_behaves_like_directory,
        multiple_sources,
        relative_root.as_deref(),
        metadata_start,
    )?;

    let metadata = match metadata_result {
        SourceMetadataResult::Found(m) => m,
        SourceMetadataResult::Handled => return Ok(()),
        SourceMetadataResult::NotFoundError(error) => {
            return Err(LocalCopyError::io(
                "access source",
                source_path.to_path_buf(),
                error,
            ));
        }
        SourceMetadataResult::IoError(error) => {
            return Err(LocalCopyError::io(
                "access source",
                source_path.to_path_buf(),
                error,
            ));
        }
    };

    context.record_file_list_generation(metadata_start.elapsed());

    // upstream: flist.c:make_file() - skip files with bogus zero st_mode
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() == 0 {
            context.record_io_error();
            return Ok(());
        }
    }

    let file_type = metadata.file_type();

    let destination_base = if let Some(parent) = &relative_parent {
        destination_path.join(parent)
    } else {
        destination_path.to_path_buf()
    };

    let root_device = if context.one_file_system_enabled() {
        device_identifier(source_path, &metadata)
    } else {
        None
    };

    // With -xx (level >= 2), skip root-level source directories that are mount
    // points - i.e. their device ID differs from their parent directory.
    if context.one_file_system_level() >= 2 && file_type.is_dir() {
        if let Some(parent) = source_path.parent() {
            if let Ok(parent_meta) = fs::symlink_metadata(parent) {
                if let Some(parent_dev) = device_identifier(parent, &parent_meta) {
                    if let Some(source_dev) = root_device {
                        if source_dev != parent_dev {
                            let record_relative = relative_root
                                .as_deref()
                                .and_then(|p| non_empty_path(p))
                                .or_else(|| source_path.file_name().map(Path::new));
                            context.record_skipped_mount_point(record_relative);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    let proc_ctx = SourceProcessingContext {
        source,
        source_path,
        metadata: metadata.clone(),
        file_type,
        relative_root: relative_root.clone(),
        relative_parent: relative_parent.clone(),
        destination_path,
        destination_base,
        destination_behaves_like_directory,
        multiple_sources,
        root_device,
    };

    let record_relative = proc_ctx.compute_record_relative();
    context.record_file_list_entry(record_relative.as_deref());

    if proc_ctx.requires_directory_destination() && !destination_behaves_like_directory {
        return Err(LocalCopyError::invalid_argument(
            LocalCopyArgumentError::DestinationMustBeDirectory,
        ));
    }

    if file_type.is_dir() {
        if source.copy_contents() {
            handle_directory_contents_copy(
                context,
                source_path,
                &metadata,
                relative_root.as_ref(),
                destination_path,
                root_device,
            )?;
        } else {
            handle_directory_copy(
                context,
                source_path,
                &metadata,
                relative_root.as_ref(),
                destination_path,
                &proc_ctx.destination_base,
                destination_behaves_like_directory,
                multiple_sources,
                root_device,
            )?;
        }
    } else {
        handle_non_directory_source(context, &proc_ctx, plan)?;
    }

    context.enforce_timeout()?;
    Ok(())
}

/// Flushes all deferred operations after source processing is complete.
fn flush_deferred_operations(context: &mut CopyContext) -> Result<(), LocalCopyError> {
    context.flush_deferred_updates()?;
    context.flush_deferred_deletions()?;
    context.flush_deferred_syncs()?;
    context.enforce_timeout()?;
    Ok(())
}

/// Deletes a destination entry whose source has gone missing.
///
/// Invoked when `--delete-missing-args` is active and the source path
/// no longer exists on disk.
pub(super) fn delete_missing_source_entry(
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

    if let Some(limit) = context.options().max_deletion_limit()
        && context.summary().items_deleted() >= limit
    {
        return Err(LocalCopyError::delete_limit_exceeded(1));
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
