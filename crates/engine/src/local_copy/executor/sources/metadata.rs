//! Source metadata fetching, symlink resolution, and relative path computation.

use std::borrow::Cow;
use std::fs::{FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::local_copy::{CopyContext, LocalCopyError, SourceSpec};

use super::super::follow_symlink_metadata;
use super::orchestration::delete_missing_source_entry;
use super::types::SourceMetadataResult;

/// Whether this operand's trailing DOTDIR marker still governs its own stat.
///
/// Under `--relative` the marker rides on the name upstream stats directly
/// (`flist.c:2652-2657` strips it and records it in `name_type`). Without
/// `--relative` upstream splits the operand at its last `/` (`flist.c:2607-2618`),
/// so a trailing marker makes the WHOLE operand the directory
/// `change_pathname()` chdir()s into before `.` is stat'd - and that chdir
/// follows symlinks and demands a real directory, which is exactly what the
/// unstripped `lstat("operand/")` already reports.
fn marker_governs_operand_stat(context: &CopyContext, source: &SourceSpec) -> bool {
    context.relative_paths_enabled() && source.copy_contents()
}

/// Returns the path a source operand is stat'd and read through, with any
/// trailing DOTDIR marker removed.
///
/// Upstream keeps TWO facts about a command-line operand apart: the name it
/// works on (`fbuf`, stripped of the marker at `flist.c:2652-2657`) and
/// `name_type`, the marker itself (`flist.c:115-118` - `NORMAL_NAME`,
/// `SLASH_ENDING_NAME`, `DOTDIR_NAME`). `SourceSpec::copy_contents` is oc's
/// `name_type != NORMAL_NAME`, so the marker survives the strip.
///
/// The strip matters because `lstat("sym-to-dir/")` is not `lstat("sym-to-dir")`:
/// the kernel resolves the trailing slash, so the raw form silently behaves like
/// a `stat()` that additionally demands a directory. That turns a symlink to a
/// file into `ENOTDIR` and a dangling symlink into `ENOENT`, where upstream
/// transfers the symlink itself.
///
/// Scoped by [`marker_governs_operand_stat`].
pub(super) fn operand_stat_path<'a>(
    context: &CopyContext,
    source: &'a SourceSpec,
) -> Cow<'a, Path> {
    let path = source.path();
    if !marker_governs_operand_stat(context, source) {
        return Cow::Borrowed(path);
    }

    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() > 1 && (bytes.ends_with(b"/") || bytes.ends_with(b"/.")) {
        Cow::Owned(path.components().collect())
    } else {
        Cow::Borrowed(path)
    }
}

/// Stats a source operand the way upstream's `link_stat()` does.
///
/// `flist.c:2697` feeds the operand's marker into the stat as the second
/// disjunct of `follow_dirlinks`:
///
/// ```text
/// link_stat(fbuf, &st, copy_dirlinks || name_type != NORMAL_NAME)
/// ```
///
/// and `link_stat()` itself (`flist.c:286-301`) is a two-step, not one
/// decision: it lstat()s first, and replaces that result with the stat ONLY
/// when the target is a directory. A symlink to a file, a dangling symlink, or
/// a plain non-directory operand therefore keeps its lstat result and is
/// transferred as itself - which is what distinguishes this from `--copy-links`.
///
/// Upstream's `copy_links` short-circuit (`flist.c:289`) is applied only to a
/// marked operand. Every other operand keeps reaching
/// [`resolve_effective_metadata`], which already reproduces both it and the
/// dir-only `--copy-dirlinks` rule from the lstat result returned here; the
/// marked operand cannot, because a `--copy-links` stat that FAILS is an
/// operand-level `link_stat` failure (exit 23) upstream, not a file that
/// vanished mid-transfer (exit 24).
fn operand_link_stat(
    context: &CopyContext,
    source: &SourceSpec,
    source_path: &Path,
) -> io::Result<Metadata> {
    if context.copy_links_enabled() && marker_governs_operand_stat(context, source) {
        return std::fs::metadata(source_path);
    }

    let metadata = std::fs::symlink_metadata(source_path)?;

    let follow_dirlinks = context.copy_dirlinks_enabled() || source.copy_contents();
    if follow_dirlinks && metadata.file_type().is_symlink() {
        if let Ok(followed) = std::fs::metadata(source_path) {
            if followed.file_type().is_dir() {
                return Ok(followed);
            }
        }
    }

    Ok(metadata)
}

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
    match operand_link_stat(context, source, source_path) {
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
