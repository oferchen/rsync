//! Types used during source enumeration and processing.

use std::fs::{FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use crate::local_copy::SourceSpec;

use super::super::non_empty_path;

/// Tracks whether the destination path exists and what kind of entry it is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DestinationState {
    pub(super) exists: bool,
    pub(super) is_dir: bool,
    pub(super) symlink_to_dir: bool,
}

/// Context for processing a single source entry.
///
/// Captures all computed values needed to process one source,
/// reducing parameter passing between helper functions.
pub(super) struct SourceProcessingContext<'a> {
    pub(super) source: &'a SourceSpec,
    pub(super) source_path: &'a Path,
    pub(super) metadata: Metadata,
    pub(super) file_type: FileType,
    pub(super) relative_root: Option<PathBuf>,
    pub(super) relative_parent: Option<PathBuf>,
    pub(super) destination_path: &'a Path,
    pub(super) destination_base: PathBuf,
    pub(super) destination_behaves_like_directory: bool,
    pub(super) multiple_sources: bool,
    pub(super) root_device: Option<u64>,
}

impl<'a> SourceProcessingContext<'a> {
    /// Computes the relative path for recording this source entry.
    pub(super) fn compute_record_relative(&self) -> Option<PathBuf> {
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
    pub(super) fn requires_directory_destination(&self) -> bool {
        self.relative_parent.is_some()
            || (self.relative_root.is_some()
                && (self.source.copy_contents() || self.file_type.is_dir()))
    }
}

/// Result of fetching source metadata with error handling.
pub(super) enum SourceMetadataResult {
    /// Metadata was successfully retrieved.
    Found(Metadata),
    /// Source was not found but was handled (deleted or ignored).
    Handled,
    /// Source was not found and should be reported as an error.
    NotFoundError(io::Error),
    /// Other I/O error occurred.
    IoError(io::Error),
}
