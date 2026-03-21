//! File-type-specific copy handlers for source entries.

use std::fs;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyArgumentError, LocalCopyError, LocalCopyMetadata,
    LocalCopyPlan, LocalCopyRecord,
};
use crate::local_copy::{is_device, is_fifo};
use ::metadata::MetadataOptions;

use super::super::{
    copy_device, copy_directory_recursive, copy_fifo, copy_file, copy_symlink, non_empty_path,
};
use super::destination::{compute_special_target_path, compute_target_path};
use super::metadata::resolve_effective_metadata;
use super::types::SourceProcessingContext;

/// Records a skipped symlink in the copy context.
///
/// Centralizes the "skip symlink" behavior so the main copy loop
/// stays focused on control flow and delegates reporting details.
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

/// Handles copying a directory source that should have its contents copied.
///
/// This is for sources specified with a trailing slash (copy contents, not
/// the directory itself). Returns true if the source was handled.
pub(super) fn handle_directory_contents_copy(
    context: &mut CopyContext,
    source_path: &Path,
    metadata: &Metadata,
    relative_root: Option<&PathBuf>,
    destination_path: &Path,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    let recursion_enabled = context.recursive_enabled();
    let dirs_enabled = context.dirs_enabled();

    // upstream: flist.c:flist_sort_and_clean() - when -m is active,
    // directories excluded by non-dir-specific rules are still
    // traversed so file-level include rules can rescue contents.
    if let Some(root) = relative_root {
        if !(context.allows(root.as_path(), true)
            || context.prune_empty_dirs_enabled()
                && context.excluded_dir_by_non_dir_rule(root.as_path()))
        {
            return Ok(true);
        }
    }

    if !recursion_enabled && !dirs_enabled {
        let skip_relative = relative_root.and_then(|root| non_empty_path(root.as_path()));
        context.summary_mut().record_directory_total();
        context.record_skipped_directory(skip_relative);
        return Ok(true);
    }

    let mut target_root = destination_path.to_path_buf();
    if let Some(root) = relative_root {
        target_root = destination_path.join(root);
    }

    // Trailing-slash copy: contents become the transfer root directly,
    // so no extra prefix in the relative path.
    context.set_safety_depth_offset(0);

    copy_directory_recursive(
        context,
        source_path,
        &target_root,
        metadata,
        relative_root.and_then(|root| non_empty_path(root.as_path())),
        root_device,
    )?;

    Ok(true)
}

/// Handles copying a directory source (the directory itself, not just contents).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_directory_copy(
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
        // upstream: flist.c:flist_sort_and_clean() - when -m is active,
        // directories excluded by non-dir-specific rules are still
        // traversed so file-level include rules can rescue their contents.
        if !(context.prune_empty_dirs_enabled() && context.excluded_dir_by_non_dir_rule(&relative))
        {
            return Ok(());
        }
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

    // Non-trailing-slash copy: the relative path starts with the source
    // directory name, which inflates the depth seen by safe-links checks.
    // Record an offset of 1 so that safety-relative paths exclude it.
    context.set_safety_depth_offset(1);

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

/// Handles copying a symlink source.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_symlink_copy(
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
///
/// Returns true if the caller should continue to the next source.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_fifo_copy(
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
        return Ok(true);
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

    Ok(false)
}

/// Handles copying a device node source.
///
/// Returns true if the caller should continue to the next source.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_device_copy(
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
        let _ = copy_file(context, source_path, &target, metadata, record_path)?;
    } else if !context.devices_enabled() {
        context.record_skipped_non_regular(record_path);
        return Ok(true);
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

    Ok(false)
}

/// Handles copying a non-directory source (file, symlink, device, FIFO).
pub(super) fn handle_non_directory_source(
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

    let (effective_metadata, effective_type) =
        resolve_effective_metadata(context, source_path, metadata, file_type)?;

    if !context.allows(&relative, effective_type.is_dir()) {
        // upstream: flist.c:flist_sort_and_clean() - when -m is active,
        // directories excluded by non-dir-specific rules are still
        // traversed so file-level include rules can rescue contents.
        if !(effective_type.is_dir()
            && context.prune_empty_dirs_enabled()
            && context.excluded_dir_by_non_dir_rule(&relative))
        {
            return Ok(());
        }
    }

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
        let _ = copy_file(
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
    } else if file_type.is_symlink() && effective_type.is_symlink() {
        // Only preserve as a symlink when --copy-links did NOT resolve the
        // referent. When copy_links is active and the target is a special
        // file (FIFO / device), the effective_type will differ from
        // file_type and we must fall through to the FIFO / device branches.
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
