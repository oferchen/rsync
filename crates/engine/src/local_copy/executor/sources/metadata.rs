//! Source metadata fetching, symlink resolution, and relative path computation.

use std::fs::{FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::local_copy::{CopyContext, LocalCopyError, SourceSpec};

use super::super::follow_symlink_metadata;
use super::orchestration::delete_missing_source_entry;
use super::types::SourceMetadataResult;

/// Attempts to fetch metadata for a source path, handling missing source scenarios.
///
/// When the source is missing, this delegates to `--delete-missing-args` or
/// `--ignore-missing-args` behavior as appropriate.
#[allow(clippy::too_many_arguments)]
pub(super) fn fetch_source_metadata(
    context: &mut CopyContext,
    source: &SourceSpec,
    source_path: &Path,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    relative_root: Option<&Path>,
    metadata_start: Instant,
) -> Result<SourceMetadataResult, LocalCopyError> {
    match std::fs::symlink_metadata(source_path) {
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
pub(super) fn compute_relative_paths(
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

/// Resolves the effective metadata for a source, following symlinks if configured.
///
/// When `--copy-links` or `--copy-dirlinks` is active, symlink targets are
/// resolved and their metadata returned instead.
pub(super) fn resolve_effective_metadata(
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
