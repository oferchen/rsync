//! Handling of reference directories and link-dest decisions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::local_copy::{CopyContext, LocalCopyError, ReferenceDirectoryKind};
use oc_rsync_meta::MetadataOptions;

use super::{CopyComparison, should_skip_copy};

pub(crate) enum ReferenceDecision {
    Skip,
    Copy(PathBuf),
    Link(PathBuf),
}

pub(crate) fn resolve_reference_candidate(
    base: &Path,
    relative: &Path,
    destination: &Path,
) -> PathBuf {
    if base.is_absolute() {
        base.join(relative)
    } else {
        let mut ancestor = destination.to_path_buf();
        let depth = relative.components().count();
        for _ in 0..depth {
            if !ancestor.pop() {
                break;
            }
        }
        ancestor.join(base).join(relative)
    }
}

pub(crate) struct ReferenceQuery<'a> {
    pub(crate) destination: &'a Path,
    pub(crate) relative: &'a Path,
    pub(crate) source: &'a Path,
    pub(crate) metadata: &'a fs::Metadata,
    pub(crate) metadata_options: &'a MetadataOptions,
    pub(crate) size_only: bool,
    pub(crate) checksum: bool,
}

pub(crate) fn find_reference_action(
    context: &CopyContext<'_>,
    query: ReferenceQuery<'_>,
) -> Result<Option<ReferenceDecision>, LocalCopyError> {
    let ReferenceQuery {
        destination,
        relative,
        source,
        metadata,
        metadata_options,
        size_only,
        checksum,
    } = query;
    for reference in context.reference_directories() {
        let candidate = resolve_reference_candidate(reference.path(), relative, destination);
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference file",
                    candidate,
                    error,
                ));
            }
        };

        if !candidate_metadata.file_type().is_file() {
            continue;
        }

        if should_skip_copy(CopyComparison {
            source_path: source,
            source: metadata,
            destination_path: &candidate,
            destination: &candidate_metadata,
            options: metadata_options,
            size_only,
            checksum,
            checksum_algorithm: context.options().checksum_algorithm(),
            modify_window: context.options().modify_window(),
        }) {
            return Ok(Some(match reference.kind() {
                ReferenceDirectoryKind::Compare => ReferenceDecision::Skip,
                ReferenceDirectoryKind::Copy => ReferenceDecision::Copy(candidate),
                ReferenceDirectoryKind::Link => ReferenceDecision::Link(candidate),
            }));
        }
    }

    Ok(None)
}
