use std::fs;
use std::fs::{FileType, Metadata};
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
use ::metadata::MetadataOptions;

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

/// Context for processing a single source entry.
///
/// This struct captures all the computed values needed to process one source,
/// reducing parameter passing between helper functions.
struct SourceProcessingContext<'a> {
    source: &'a SourceSpec,
    source_path: &'a Path,
    metadata: Metadata,
    file_type: FileType,
    relative_root: Option<PathBuf>,
    relative_parent: Option<PathBuf>,
    destination_path: &'a Path,
    destination_base: PathBuf,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    root_device: Option<u64>,
}

impl<'a> SourceProcessingContext<'a> {
    /// Computes the relative path for recording this source entry.
    fn compute_record_relative(&self) -> Option<PathBuf> {
        if self.file_type.is_dir() && self.source.copy_contents() {
            None
        } else if let Some(root) = self.relative_root.as_ref() {
            non_empty_path(root.as_path()).map(Path::to_path_buf)
        } else {
            self.source_path
                .file_name()
                .map(|name| PathBuf::from(Path::new(name)))
        }
    }

    /// Determines if a directory destination is required for this source.
    fn requires_directory_destination(&self) -> bool {
        self.relative_parent.is_some()
            || (self.relative_root.is_some()
                && (self.source.copy_contents() || self.file_type.is_dir()))
    }
}

/// Result of fetching source metadata with error handling.
enum SourceMetadataResult {
    /// Metadata was successfully retrieved.
    Found(Metadata),
    /// Source was not found but was handled (deleted or ignored).
    Handled,
    /// Source was not found and should be reported as an error.
    NotFoundError(io::Error),
    /// Other I/O error occurred.
    IoError(io::Error),
}

/// Attempts to fetch metadata for a source path, handling missing source scenarios.
#[allow(clippy::too_many_arguments)]
fn fetch_source_metadata(
    context: &mut CopyContext,
    source: &SourceSpec,
    source_path: &Path,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    relative_root: Option<&Path>,
    metadata_start: Instant,
) -> Result<SourceMetadataResult, LocalCopyError> {
    match fs::symlink_metadata(source_path) {
        Ok(metadata) => Ok(SourceMetadataResult::Found(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            context.record_file_list_generation(metadata_start.elapsed());

            if context.delete_missing_args_enabled() {
                delete_missing_source_entry(
                    context,
                    source,
                    destination_path,
                    destination_behaves_like_directory,
                    multiple_sources,
                    relative_root,
                )?;
                Ok(SourceMetadataResult::Handled)
            } else if context.ignore_missing_args_enabled() {
                Ok(SourceMetadataResult::Handled)
            } else {
                Ok(SourceMetadataResult::NotFoundError(error))
            }
        }
        Err(error) => Ok(SourceMetadataResult::IoError(error)),
    }
}

/// Computes relative root and parent paths for a source entry.
fn compute_relative_paths(
    context: &CopyContext,
    source: &SourceSpec,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let relative_enabled = context.relative_paths_enabled();
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

    (relative_root, relative_parent)
}

/// Handles copying a directory source that should have its contents copied.
///
/// This is for sources specified with a trailing slash (copy contents, not the directory itself).
/// Returns true if the source was handled (caller should continue to next source).
fn handle_directory_contents_copy(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &Metadata,
    relative_root: Option<&PathBuf>,
    destination_path: &Path,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    let recursion_enabled = context.recursive_enabled();
    let dirs_enabled = context.dirs_enabled();

    // Check filter rules
    if let Some(root) = relative_root {
        if !context.allows(root.as_path(), true) {
            return Ok(true); // Filtered out, continue to next source
        }
    }

    // Skip if recursion/dirs not enabled
    if !recursion_enabled && !dirs_enabled {
        let skip_relative = relative_root.and_then(|root| non_empty_path(root.as_path()));
        context.summary_mut().record_directory_total();
        context.record_skipped_directory(skip_relative);
        return Ok(true); // Skipped, continue to next source
    }

    // Compute target directory
    let mut target_root = destination_path.to_path_buf();
    if let Some(root) = relative_root {
        target_root = destination_path.join(root);
    }

    copy_directory_recursive(
        context,
        source_path,
        &target_root,
        metadata,
        relative_root.and_then(|root| non_empty_path(root.as_path())),
        root_device,
    )?;

    Ok(true) // Handled, continue to next source
}

/// Handles copying a directory source (the directory itself, not just contents).
#[allow(clippy::too_many_arguments)]
fn handle_directory_copy(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &Metadata,
    relative_root: Option<&PathBuf>,
    destination_path: &Path,
    destination_base: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    root_device: Option<u64>,
) -> Result<(), LocalCopyError> {
    let recursion_enabled = context.recursive_enabled();
    let dirs_enabled = context.dirs_enabled();

    let name = source_path.file_name().ok_or_else(|| {
        LocalCopyError::invalid_argument(LocalCopyArgumentError::DirectoryNameUnavailable)
    })?;

    let relative = relative_root
        .cloned()
        .unwrap_or_else(|| PathBuf::from(Path::new(name)));

    if !context.allows(&relative, true) {
        return Ok(());
    }

    if !recursion_enabled && !dirs_enabled {
        context.summary_mut().record_directory_total();
        let record_relative = non_empty_path(relative.as_path());
        context.record_skipped_directory(record_relative);
        return Ok(());
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
        metadata,
        non_empty_path(relative.as_path()),
        root_device,
    )?;
    Ok(())
}

/// Resolves the effective metadata for a source, following symlinks if configured.
fn resolve_effective_metadata(
    context: &CopyContext,
    source_path: &Path,
    original_metadata: &Metadata,
    original_file_type: FileType,
) -> Result<(Metadata, FileType), LocalCopyError> {
    if !original_file_type.is_symlink() {
        return Ok((original_metadata.clone(), original_file_type));
    }

    if !context.copy_links_enabled() && !context.copy_dirlinks_enabled() {
        return Ok((original_metadata.clone(), original_file_type));
    }

    match follow_symlink_metadata(source_path) {
        Ok(target_metadata) => {
            let target_type = target_metadata.file_type();
            if context.copy_links_enabled()
                || (context.copy_dirlinks_enabled() && target_type.is_dir())
            {
                Ok((target_metadata, target_type))
            } else {
                Ok((original_metadata.clone(), original_file_type))
            }
        }
        Err(error) => {
            if context.copy_links_enabled() {
                Err(error)
            } else {
                Ok((original_metadata.clone(), original_file_type))
            }
        }
    }
}

/// Computes the target path for a non-directory entry.
fn compute_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
    is_directory: bool,
) -> PathBuf {
    // Simplified non-minimal boolean expression:
    // destination_behaves_like_directory
    //   && !(prefer_root_destination && !is_directory)
    // =>
    // destination_behaves_like_directory
    //   && (!prefer_root_destination || is_directory)
    if destination_behaves_like_directory && (!prefer_root_destination || is_directory) {
        destination_base.join(name)
    } else {
        destination_path.to_path_buf()
    }
}

/// Computes the target path for special entries (symlinks, FIFOs, devices).
///
/// These entries don't use the directory-specific logic that regular files use.
fn compute_special_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
) -> PathBuf {
    if destination_behaves_like_directory && !prefer_root_destination {
        destination_base.join(name)
    } else {
        destination_path.to_path_buf()
    }
}

/// Handles copying a symlink source.
#[allow(clippy::too_many_arguments)]
fn handle_symlink_copy(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &Metadata,
    metadata_options: &MetadataOptions,
    record_path: Option<&Path>,
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
) -> Result<(), LocalCopyError> {
    if context.links_enabled() {
        let target = compute_special_target_path(
            destination_path,
            destination_base,
            name,
            destination_behaves_like_directory,
            prefer_root_destination,
        );

        copy_symlink(
            context,
            source_path,
            &target,
            metadata,
            metadata_options,
            record_path,
        )
    } else {
        record_skipped_symlink(context, source_path, metadata, record_path);
        Ok(())
    }
}

/// Handles copying a FIFO (named pipe) source.
/// Returns true if the caller should continue to the next source.
#[allow(clippy::too_many_arguments)]
fn handle_fifo_copy(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &Metadata,
    metadata_options: &MetadataOptions,
    record_path: Option<&Path>,
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
) -> Result<bool, LocalCopyError> {
    if !context.specials_enabled() {
        context.record_skipped_non_regular(record_path);
        return Ok(true); // Continue to next source
    }

    let target = compute_special_target_path(
        destination_path,
        destination_base,
        name,
        destination_behaves_like_directory,
        prefer_root_destination,
    );

    copy_fifo(
        context,
        source_path,
        &target,
        metadata,
        metadata_options,
        record_path,
    )?;

    Ok(false) // Don't skip remaining processing
}

/// Handles copying a device node source.
/// Returns true if the caller should continue to the next source.
#[allow(clippy::too_many_arguments)]
fn handle_device_copy(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &Metadata,
    metadata_options: &MetadataOptions,
    record_path: Option<&Path>,
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
) -> Result<bool, LocalCopyError> {
    let target = compute_special_target_path(
        destination_path,
        destination_base,
        name,
        destination_behaves_like_directory,
        prefer_root_destination,
    );

    if context.copy_devices_as_files_enabled() {
        copy_file(context, source_path, &target, metadata, record_path)?;
    } else if !context.devices_enabled() {
        context.record_skipped_non_regular(record_path);
        return Ok(true); // Continue to next source
    } else {
        copy_device(
            context,
            source_path,
            &target,
            metadata,
            metadata_options,
            record_path,
        )?;
    }

    Ok(false) // Don't skip remaining processing
}

/// Handles copying a non-directory source (file, symlink, device, FIFO).
fn handle_non_directory_source(
    context: &mut CopyContext,
    proc_ctx: &SourceProcessingContext<'_>,
    plan: &LocalCopyPlan,
) -> Result<(), LocalCopyError> {
    let source_path = proc_ctx.source_path;
    let metadata = &proc_ctx.metadata;
    let file_type = proc_ctx.file_type;

    let name = source_path.file_name().ok_or_else(|| {
        LocalCopyError::invalid_argument(LocalCopyArgumentError::FileNameUnavailable)
    })?;

    let relative = proc_ctx
        .relative_root
        .clone()
        .unwrap_or_else(|| PathBuf::from(Path::new(name)));

    // Resolve effective metadata (following symlinks if configured)
    let (effective_metadata, effective_type) =
        resolve_effective_metadata(context, source_path, metadata, file_type)?;

    if !context.allows(&relative, effective_type.is_dir()) {
        return Ok(());
    }

    // Determine if we should use the root destination directly
    let prefer_root_destination = proc_ctx.destination_behaves_like_directory
        && context.force_replacements_enabled()
        && !proc_ctx.multiple_sources
        && !plan.destination_spec().force_directory()
        && proc_ctx.relative_parent.is_none()
        && !proc_ctx.source.copy_contents();

    let record_path = non_empty_path(relative.as_path());
    let metadata_options = context.metadata_options();

    if effective_type.is_file() {
        let target = compute_target_path(
            proc_ctx.destination_path,
            &proc_ctx.destination_base,
            name,
            proc_ctx.destination_behaves_like_directory,
            prefer_root_destination,
            false,
        );
        copy_file(
            context,
            source_path,
            &target,
            &effective_metadata,
            record_path,
        )?;
    } else if effective_type.is_dir() {
        let target = compute_target_path(
            proc_ctx.destination_path,
            &proc_ctx.destination_base,
            name,
            proc_ctx.destination_behaves_like_directory,
            prefer_root_destination,
            true,
        );
        copy_directory_recursive(
            context,
            source_path,
            &target,
            &effective_metadata,
            non_empty_path(relative.as_path()),
            proc_ctx.root_device,
        )?;
    } else if file_type.is_symlink() {
        handle_symlink_copy(
            context,
            source_path,
            metadata,
            &metadata_options,
            record_path,
            proc_ctx.destination_path,
            &proc_ctx.destination_base,
            name,
            proc_ctx.destination_behaves_like_directory,
            prefer_root_destination,
        )?;
    } else if is_fifo(effective_type) {
        if handle_fifo_copy(
            context,
            source_path,
            &effective_metadata,
            &metadata_options,
            record_path,
            proc_ctx.destination_path,
            &proc_ctx.destination_base,
            name,
            proc_ctx.destination_behaves_like_directory,
            prefer_root_destination,
        )? {
            return Ok(());
        }
    } else if is_device(effective_type) {
        if handle_device_copy(
            context,
            source_path,
            &effective_metadata,
            &metadata_options,
            record_path,
            proc_ctx.destination_path,
            &proc_ctx.destination_base,
            name,
            proc_ctx.destination_behaves_like_directory,
            prefer_root_destination,
        )? {
            return Ok(());
        }
    } else {
        return Err(LocalCopyError::invalid_argument(
            LocalCopyArgumentError::UnsupportedFileType,
        ));
    }

    Ok(())
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
    context.enforce_timeout()?;

    let source_path = source.path();
    let metadata_start = Instant::now();

    // Compute relative paths
    let (relative_root, relative_parent) = compute_relative_paths(context, source);

    // Fetch and validate source metadata
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

    let file_type = metadata.file_type();

    // Build processing context
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

    // Record the file list entry
    let record_relative = proc_ctx.compute_record_relative();
    context.record_file_list_entry(record_relative.as_deref());

    // Validate directory destination requirement
    if proc_ctx.requires_directory_destination() && !destination_behaves_like_directory {
        return Err(LocalCopyError::invalid_argument(
            LocalCopyArgumentError::DestinationMustBeDirectory,
        ));
    }

    // Process based on file type
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

    // Flush any remaining deferred sync operations
    #[cfg(feature = "batch-sync")]
    context.flush_deferred_syncs()?;

    context.enforce_timeout()?;
    Ok(())
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

            for source in plan.sources() {
                process_single_source(
                    context,
                    plan,
                    source,
                    destination_path,
                    destination_behaves_like_directory,
                    multiple_sources,
                )?;
            }

            flush_deferred_operations(context)?;
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
